//! Headless `egui` tests: synthetic pointer and key events drive the panel
//! through real passes.

use egui::{Event, Key, PointerButton, Pos2, RawInput, Rect, Ui, UiBuilder, Vec2, vec2};
use fm_grade::{CurveChannel, WheelKind, builtin_preset};
use fm_ui_model::grade_editor::{
    CompareMode, CurveTarget, GradeDocument, GradeParam, ScopeKind, sample_still,
};
use fm_ui_model::i18n::{Language, Locale};

use super::*;

const FRAME_SECONDS: f64 = 1.0 / 60.0;

struct Harness {
    context: egui::Context,
    document: GradeDocument,
    state: ColorPanelState,
    time: f64,
    modifiers: egui::Modifiers,
    locale: Option<Locale>,
}

impl Harness {
    fn new(mode: PanelMode) -> Self {
        let mut state = ColorPanelState::with_source(&sample_still(320, 180));
        state.mode = mode;
        Self {
            context: egui::Context::default(),
            document: GradeDocument::default(),
            state,
            time: 0.0,
            modifiers: egui::Modifiers::NONE,
            locale: None,
        }
    }

    fn frame(&mut self, events: Vec<Event>) -> ColorPanelOutput {
        self.context.begin_pass(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_400.0, 900.0))),
            time: Some(self.time),
            modifiers: self.modifiers,
            events,
            ..RawInput::default()
        });
        self.time += FRAME_SECONDS;
        let mut ui = Ui::new(
            self.context.clone(),
            egui::Id::new("fm-ui-egui-color-panel-test"),
            UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(self.context.viewport_rect()),
        );
        let mut widget = ColorPanelWidget::new(&mut self.document, &mut self.state);
        if let Some(locale) = self.locale {
            widget = widget.locale(locale);
        }
        let output = widget.show(&mut ui);
        let _ = self.context.end_pass();
        output
    }

    fn idle(&mut self) -> ColorPanelOutput {
        self.frame(Vec::new())
    }

    fn button(&mut self, at: Pos2, button: PointerButton, pressed: bool) -> ColorPanelOutput {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton {
                pos: at,
                button,
                pressed,
                modifiers: self.modifiers,
            },
        ])
    }

    fn press(&mut self, at: Pos2) -> ColorPanelOutput {
        self.button(at, PointerButton::Primary, true)
    }

    fn release(&mut self, at: Pos2) -> ColorPanelOutput {
        self.button(at, PointerButton::Primary, false)
    }

    fn move_to(&mut self, at: Pos2) -> ColorPanelOutput {
        self.frame(vec![Event::PointerMoved(at)])
    }

    fn click(&mut self, at: Pos2) -> ColorPanelOutput {
        self.press(at);
        self.release(at)
    }

    fn double_click(&mut self, at: Pos2) -> ColorPanelOutput {
        self.click(at);
        self.click(at)
    }

    /// Presses at `from`, moves to `to` in a few steps, and releases.
    fn drag(&mut self, from: Pos2, to: Pos2) -> ColorPanelOutput {
        self.press(from);
        for step in 1..=4_u8 {
            self.move_to(from + (to - from) * (f32::from(step) / 4.0));
        }
        self.release(to)
    }

    fn key(&mut self, key: Key, modifiers: egui::Modifiers) -> ColorPanelOutput {
        let output = self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]);
        self.idle();
        output
    }
}

fn wheel_rect(output: &ColorPanelOutput, kind: WheelKind) -> Rect {
    output
        .rects
        .wheels
        .iter()
        .find(|(wheel, _)| *wheel == kind)
        .unwrap()
        .1
}

fn slider_rect(output: &ColorPanelOutput, param: GradeParam) -> Rect {
    output
        .rects
        .sliders
        .iter()
        .find(|(slider, _)| *slider == param)
        .unwrap()
        .1
}

fn action_rect(output: &ColorPanelOutput, action: GradeAction) -> Rect {
    output
        .rects
        .actions
        .iter()
        .find(|(key, _)| *key == action)
        .unwrap()
        .1
}

/// The screen point of normalized curve coordinates.
fn curve_point(output: &ColorPanelOutput, at: [f32; 2]) -> Pos2 {
    let inner = output.rects.curve.unwrap().shrink(8.0);
    Pos2::new(
        inner.min.x + at[0] * inner.width(),
        inner.max.y - at[1] * inner.height(),
    )
}

#[test]
fn wheel_drag_changes_the_grade_in_one_undo_step() {
    let mut harness = Harness::new(PanelMode::Advanced);
    let output = harness.idle();
    let wheel = wheel_rect(&output, WheelKind::Gain);
    let radius = wheel.width() / 2.0 - 3.0;
    let centre = wheel.center();
    let output = harness.drag(centre, centre + vec2(radius * 0.5, -radius * 0.25));
    assert!(output.changed || harness.document.undo_depth() == 1);
    let gain = harness.document.grade().wheels.gain;
    assert!((450..=550).contains(&gain.x), "{gain:?}");
    assert!((200..=300).contains(&gain.y), "{gain:?}");
    assert_eq!(gain.master, 0);
    assert_eq!(harness.document.undo_depth(), 1, "a whole drag is one step");
    assert!(!harness.document.gesture_active());

    // Shift drags finely: the same motion moves a tenth as far.
    harness.modifiers = egui::Modifiers::SHIFT;
    harness.drag(centre, centre + vec2(radius * 0.5, 0.0));
    harness.modifiers = egui::Modifiers::NONE;
    let fine = harness.document.grade().wheels.gain;
    assert!(
        (40..=60).contains(&(fine.x - gain.x)),
        "{gain:?} -> {fine:?}"
    );
    assert_eq!(harness.document.undo_depth(), 2);

    // Double-click recentres the colour and keeps the master.
    harness
        .document
        .set_param(GradeParam::WheelMaster(WheelKind::Gain), 300)
        .unwrap();
    harness.double_click(centre);
    let reset = harness.document.grade().wheels.gain;
    assert_eq!((reset.x, reset.y, reset.master), (0, 0, 300));

    harness.document.undo().unwrap();
    harness.document.undo().unwrap();
    harness.document.undo().unwrap();
    assert_eq!(harness.document.grade().wheels.gain, gain);
}

#[test]
fn curve_points_add_drag_and_delete() {
    let mut harness = Harness::new(PanelMode::Advanced);
    let output = harness.idle();
    let master = CurveTarget::Rgb(CurveChannel::Master);
    // Click adds a point.
    harness.click(curve_point(&output, [0.25, 0.6]));
    assert_eq!(master.len(harness.document.grade()), 3);
    assert_eq!(harness.document.undo_depth(), 1);
    let added = master.points(harness.document.grade())[1];
    assert!(
        (added[0] - 0.25).abs() < 0.01 && (added[1] - 0.6).abs() < 0.01,
        "{added:?}"
    );

    // Dragging the point moves it (one step).
    let from = curve_point(&output, added);
    harness.drag(from, from + vec2(0.0, -20.0));
    let moved = master.points(harness.document.grade())[1];
    assert!(moved[1] > added[1] + 0.05, "{added:?} -> {moved:?}");
    assert!((moved[0] - added[0]).abs() < 0.01);
    assert_eq!(harness.document.undo_depth(), 2);

    // Dragging from empty space adds and moves a point in one step.
    let empty = curve_point(&output, [0.75, 0.75]);
    harness.drag(empty, empty + vec2(0.0, 15.0));
    assert_eq!(master.len(harness.document.grade()), 4);
    assert_eq!(harness.document.undo_depth(), 3);
    assert!(master.points(harness.document.grade())[2][1] < 0.74);

    // Right-click deletes a point; endpoints refuse with a notice.
    harness.button(curve_point(&output, moved), PointerButton::Secondary, true);
    harness.button(curve_point(&output, moved), PointerButton::Secondary, false);
    assert_eq!(master.len(harness.document.grade()), 3);
    harness.button(
        curve_point(&output, [0.0, 0.0]),
        PointerButton::Secondary,
        true,
    );
    harness.button(
        curve_point(&output, [0.0, 0.0]),
        PointerButton::Secondary,
        false,
    );
    assert_eq!(
        harness.state.notice().and_then(|notice| notice.msg_key),
        Some("grade-editor-endpoint-locked")
    );
    assert!(harness.document.grade().validate().is_ok());
}

#[test]
fn hue_curve_tab_and_seeded_points() {
    let mut harness = Harness::new(PanelMode::Advanced);
    let output = harness.idle();
    let target = CurveTarget::HUE[1];
    let tab = output
        .rects
        .curve_tabs
        .iter()
        .find(|(curve, _)| *curve == target)
        .unwrap()
        .1;
    harness.click(tab.center());
    assert_eq!(harness.state.curve, target);
    let output = harness.idle();
    harness.click(curve_point(&output, [0.1, 0.8]));
    assert_eq!(
        target.len(harness.document.grade()),
        7,
        "six anchors plus the point"
    );
    assert_eq!(harness.document.undo_depth(), 1);
}

#[test]
fn wipe_handle_drags_and_resets() {
    let mut harness = Harness::new(PanelMode::Simple);
    let output = harness.idle();
    let wipe = output
        .rects
        .compare_tabs
        .iter()
        .find(|(mode, _)| *mode == CompareMode::Wipe)
        .unwrap()
        .1;
    harness.click(wipe.center());
    assert_eq!(harness.state.compare, CompareMode::Wipe);
    let output = harness.idle();
    let handle = output.rects.wipe_handle.unwrap();
    let preview = output.rects.preview.unwrap();
    harness.drag(
        handle.center(),
        handle.center() - vec2(preview.width() * 0.25, 0.0),
    );
    assert!(
        (harness.state.wipe - 0.25).abs() < 0.03,
        "{}",
        harness.state.wipe
    );
    assert_eq!(
        harness.document.undo_depth(),
        0,
        "the wipe is a view setting"
    );
    let output = harness.idle();
    harness.double_click(output.rects.wipe_handle.unwrap().center());
    assert!((harness.state.wipe - 0.5).abs() < f32::EPSILON);
}

#[test]
fn clicking_a_look_applies_it() {
    let mut harness = Harness::new(PanelMode::Simple);
    let output = harness.idle();
    assert_eq!(output.rects.looks.len(), 6);
    let punchy = output
        .rects
        .looks
        .iter()
        .find(|(id, _)| *id == "punchy")
        .unwrap()
        .1;
    harness.click(punchy.center());
    assert_eq!(
        harness.document.matching_preset().map(|preset| preset.id),
        Some("punchy")
    );
    assert_eq!(
        harness.document.grade().saturation_permille,
        builtin_preset("punchy").unwrap().grade.saturation_permille
    );
    assert_eq!(harness.document.undo_depth(), 1);
    // Every look has a live thumbnail.
    assert_eq!(harness.state.view.thumbs.len(), 6);
}

#[test]
fn slider_drag_double_click_reset_and_auto_balance() {
    let mut harness = Harness::new(PanelMode::Simple);
    let output = harness.idle();
    let exposure = slider_rect(&output, GradeParam::Exposure);
    let track = exposure.width() - SliderLayout::FULL.label - SliderLayout::FULL.value - 4.0;
    let start = exposure.center();
    harness.drag(start, start + vec2(track * 0.25, 0.0));
    let value = harness.document.grade().exposure_millistops;
    assert!((1_900..=2_100).contains(&value), "{value}");
    assert_eq!(harness.document.undo_depth(), 1);
    harness.double_click(start);
    assert_eq!(harness.document.grade().exposure_millistops, 0);
    assert_eq!(harness.document.undo_depth(), 2);

    let auto = output.rects.auto_balance.unwrap();
    harness.click(auto.center());
    assert_eq!(harness.document.undo_depth(), 3);
    assert_ne!(
        harness.document.grade().white_balance,
        fm_grade::WhiteBalance::NEUTRAL
    );
}

#[test]
fn action_keys_and_keyboard_shortcuts() {
    let mut harness = Harness::new(PanelMode::Simple);
    let output = harness.idle();
    // Fresh: nothing to undo, paste, or reset; each says why.
    for action in [
        GradeAction::Undo,
        GradeAction::Redo,
        GradeAction::Paste,
        GradeAction::ResetAll,
    ] {
        let spec = action_spec(action, &harness.document, &harness.state, Locale::ENGLISH);
        assert!(spec.disabled_reason.is_some(), "{action:?}");
    }
    let bypass = action_rect(&output, GradeAction::Bypass);
    let output = harness.click(bypass.center());
    assert!(harness.document.is_bypassed());
    assert_eq!(output.actions, [GradeAction::Bypass]);

    // Pressing inside hands the panel the keyboard.
    harness.click(output.rects.preview.unwrap().center());
    assert!(harness.state.keyboard_active);
    harness.key(Key::B, egui::Modifiers::NONE);
    assert!(!harness.document.is_bypassed());
    harness.key(Key::W, egui::Modifiers::NONE);
    assert_eq!(harness.state.compare, CompareMode::Wipe);
    harness.key(Key::M, egui::Modifiers::NONE);
    assert_eq!(harness.state.mode, PanelMode::Advanced);
    harness.key(Key::Z, egui::Modifiers::COMMAND);
    assert!(harness.document.is_bypassed(), "undo restores the bypass");
    // A shortcut for a disabled key explains itself.
    harness.key(Key::V, egui::Modifiers::COMMAND);
    let notice = harness.state.notice().unwrap();
    assert_eq!(notice.msg_key, None);
    assert!(
        notice.text.starts_with("No grade has been copied"),
        "{}",
        notice.text
    );
    harness.key(Key::C, egui::Modifiers::COMMAND);
    assert!(harness.document.clipboard().is_some());
    // A/B: the first press copies, then flips.
    harness.key(Key::A, egui::Modifiers::NONE);
    assert_eq!(
        harness.document.active_slot(),
        fm_ui_model::grade_editor::AbSlot::B
    );

    // Pressing elsewhere releases the keyboard.
    harness.click(Pos2::new(1_390.0, 890.0));
    assert!(!harness.state.keyboard_active);
    let mode = harness.state.mode;
    harness.key(Key::M, egui::Modifiers::NONE);
    assert_eq!(harness.state.mode, mode);
}

#[test]
fn preview_follows_the_draft_and_debounces_drags() {
    let mut harness = Harness::new(PanelMode::Simple);
    harness.idle();
    assert_eq!(
        harness.state.shown(),
        Some(harness.state.key(&harness.document))
    );
    assert!(
        harness
            .state
            .scopes()
            .is_some_and(|scopes| scopes.samples > 0)
    );
    // A discrete edit shows at once.
    harness
        .document
        .set_param(GradeParam::Saturation, 0)
        .unwrap();
    harness.idle();
    assert_eq!(
        harness.state.shown(),
        Some(harness.state.key(&harness.document))
    );
    // During a drag the preview trails by at most the max wait.
    let output = harness.idle();
    let contrast = slider_rect(&output, GradeParam::Contrast);
    harness.press(contrast.center());
    let mut lagged = false;
    for step in 1..=12_u8 {
        harness.move_to(contrast.center() + vec2(f32::from(step) * 4.0, 0.0));
        lagged |= harness.state.shown() != Some(harness.state.key(&harness.document));
    }
    assert!(lagged, "drags are debounced");
    harness.release(contrast.center() + vec2(48.0, 0.0));
    harness.idle();
    assert_eq!(
        harness.state.shown(),
        Some(harness.state.key(&harness.document))
    );
    // Undo returns to an already graded picture: a cache hit.
    let (hits, _) = harness.state.preview().cache_stats();
    harness.document.undo().unwrap();
    harness.idle();
    assert!(harness.state.preview().cache_stats().0 > hits);
}

#[test]
fn scope_tabs_switch_scopes() {
    let mut harness = Harness::new(PanelMode::Simple);
    let output = harness.idle();
    for kind in ScopeKind::ALL {
        let tab = output
            .rects
            .scope_tabs
            .iter()
            .find(|(scope, _)| *scope == kind)
            .unwrap()
            .1;
        harness.click(tab.center());
        assert_eq!(harness.state.scope, kind);
    }
}

#[test]
fn spanish_wording_and_refusals() {
    let mut harness = Harness::new(PanelMode::Advanced);
    harness.locale = Some(Locale::new(Language::Spanish));
    let output = harness.idle();
    // Adding a point on an endpoint is refused, in Spanish.
    harness.click(curve_point(&output, [1.0, 0.3]));
    let notice = harness.state.notice().unwrap();
    assert_eq!(notice.msg_key, Some("grade-editor-point-taken"));
    assert!(
        notice.text.starts_with("Ya hay un punto"),
        "{}",
        notice.text
    );
    let es = Locale::new(Language::Spanish);
    assert_eq!(
        text::format_param(es, GradeParam::Exposure, 400),
        "+0,40 EV"
    );
    assert_eq!(text::format_param(es, GradeParam::Exposure, 0), "0,00 EV");
    assert_eq!(
        text::format_param(Locale::ENGLISH, GradeParam::Temperature, -250),
        "-25"
    );
    assert_eq!(
        text::format_param(Locale::ENGLISH, GradeParam::Contrast, 250),
        "+25%"
    );
    assert_eq!(text::wheel_reading(0, 500), Some((90, 50)));
}

#[test]
fn painted_glyphs_exist_in_the_default_fonts() {
    let context = egui::Context::default();
    context.begin_pass(RawInput::default());
    let font = egui::FontId::proportional(12.0);
    for glyph in ['↺', '·', '×'] {
        assert!(
            context.fonts_mut(|fonts| fonts.has_glyph(&font, glyph)),
            "missing {glyph}"
        );
    }
    let _ = context.end_pass();
}

/// Renders `setup`'s panel offscreen with the console style and returns the
/// panel's pixels (with a margin) as straight RGBA8.
fn render_panel(
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    hover: Option<Pos2>,
) -> (usize, usize, Vec<u8>) {
    use crate::offscreen::{Canvas, Textures};
    let context = egui::Context::default();
    crate::theme::apply_console_visuals(&context, false);
    let size = Vec2::new(1_200.0, 900.0);
    let mut textures = Textures::default();
    let mut last = None;
    for pass in 0..4 {
        let events = match hover {
            Some(at) if pass >= 1 => vec![Event::PointerMoved(at)],
            _ => Vec::new(),
        };
        context.begin_pass(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(f64::from(pass) * 0.5),
            events,
            ..RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-color-panel-screenshot"),
            UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(Rect::from_min_size(
                    Pos2::new(12.0, 12.0),
                    size - Vec2::splat(24.0),
                )),
        );
        let output = ColorPanelWidget::new(document, state).show(&mut ui);
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
    canvas.crop_rgba8(rect.expand(12.0))
}

/// Writes `docs/progress/color-panel.png`: the panel in Simple mode (a look
/// applied, waveform) above Advanced mode (wheels, curves, LUT, wipe
/// compare, vectorscope), rendered from the real widget.
#[test]
#[ignore = "writes docs/progress/color-panel.png; run with --ignored"]
fn write_color_panel_screenshot() {
    use fm_grade::{GradeLut, HueCurveKind, Wheel};

    let still = sample_still(480, 270);
    let mut simple = GradeDocument::default();
    simple
        .apply_preset(builtin_preset("warm").unwrap())
        .unwrap();
    simple.set_param(GradeParam::Contrast, 180).unwrap();
    let mut simple_state = ColorPanelState::with_source(&still);
    simple_state.scope = ScopeKind::Parade;
    let shot_simple = render_panel(&mut simple, &mut simple_state, None);

    let mut advanced = GradeDocument::default();
    advanced.set_param(GradeParam::Exposure, 250).unwrap();
    advanced.set_param(GradeParam::Contrast, 200).unwrap();
    advanced
        .set_wheel(WheelKind::Lift, Wheel::new(250, -120, 40))
        .unwrap();
    advanced
        .set_wheel(WheelKind::Gain, Wheel::new(-180, 160, 80))
        .unwrap();
    let master = CurveTarget::Rgb(CurveChannel::Master);
    advanced.add_curve_point(master, [0.25, 0.2]).unwrap();
    advanced.add_curve_point(master, [0.75, 0.82]).unwrap();
    advanced
        .add_curve_point(CurveTarget::Hue(HueCurveKind::HueVsSaturation), [0.6, 0.8])
        .unwrap();
    advanced
        .set_lut(Some(GradeLut::new("asset://luts/film-look.cube", 700)))
        .unwrap();
    let mut advanced_state = ColorPanelState::with_source(&still);
    advanced_state.mode = PanelMode::Advanced;
    advanced_state.compare = CompareMode::Wipe;
    advanced_state.wipe = 0.45;
    advanced_state.scope = ScopeKind::Vectorscope;
    advanced_state.lut_choices = vec!["asset://luts/film-look.cube".to_owned()];
    let shot_advanced = render_panel(&mut advanced, &mut advanced_state, None);

    let width = shot_simple.0.max(shot_advanced.0);
    let height = shot_simple.1 + shot_advanced.1;
    let background = crate::theme::graphite_color().to_array();
    let mut rgba = background.repeat(width * height);
    for (offset, (shot_width, shot_height, pixels)) in
        [(0, &shot_simple), (shot_simple.1, &shot_advanced)]
    {
        for y in 0..*shot_height {
            let source = &pixels[y * shot_width * 4..(y + 1) * shot_width * 4];
            let start = ((offset + y) * width) * 4;
            rgba[start..start + source.len()].copy_from_slice(source);
        }
    }
    let png = crate::offscreen::encode_png(width, height, &rgba);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/progress/color-panel.png");
    std::fs::write(&path, png).unwrap();
    println!("wrote {} ({width}x{height})", path.display());
}
