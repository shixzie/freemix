use egui::{Event, PointerButton, RawInput};

use super::*;
use crate::ConsoleLayout;
use crate::offscreen::{Canvas, Textures};

const SCREEN: Vec2 = Vec2::new(1_600.0, 940.0);

fn area() -> Rect {
    Rect::from_min_size(pos2(8.0, 80.0), vec2(1_584.0, 820.0))
}

/// One shell frame at `size` with `events`; returns the output.
fn frame_at(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    size: Vec2,
    events: Vec<Event>,
) -> (Vec<StudioIntent>, egui::FullOutput) {
    context.begin_pass(RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        events,
        ..RawInput::default()
    });
    let mut ui = Ui::new(
        context.clone(),
        Id::new("fm-dock-test"),
        UiBuilder::new()
            .layer_id(LayerId::background())
            .max_rect(context.viewport_rect()),
    );
    let intents = shell.draw(&mut ui, state);
    drop(ui);
    (intents, context.end_pass())
}

fn frame(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    events: Vec<Event>,
) -> egui::FullOutput {
    frame_at(context, shell, state, SCREEN, events).1
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

fn tile_rect(shell: &StudioShell, panel: PanelId) -> Rect {
    #[allow(clippy::cast_precision_loss)]
    let rect = shell.dock_ui.tile_rects.get(&panel).map(|[x, y, w, h]| {
        Rect::from_min_size(pos2(*x as f32, *y as f32), vec2(*w as f32, *h as f32))
    });
    rect.unwrap_or_else(|| panic!("{panel:?} has no tile"))
}

fn press(pos: Pos2) -> Vec<Event> {
    vec![
        Event::PointerMoved(pos),
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn release(pos: Pos2) -> Vec<Event> {
    vec![
        Event::PointerMoved(pos),
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

/// Presses at `from`, moves to `to` in steps, and releases there.
fn drag(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    from: Pos2,
    to: Pos2,
) {
    frame(context, shell, state, vec![Event::PointerMoved(from)]);
    frame(context, shell, state, press(from));
    for step in 1..=4 {
        #[allow(clippy::cast_precision_loss)]
        let at = from + (to - from) * (step as f32 / 4.0);
        frame(context, shell, state, vec![Event::PointerMoved(at)]);
    }
    frame(context, shell, state, release(to));
    frame(context, shell, state, Vec::new());
}

#[test]
fn tiles_fill_the_area_without_overlap_and_keep_minimums() {
    for preset in LayoutPreset::ALL {
        let layout = preset.layout();
        let (tiles, splitters) = layout_tiles(layout.root().unwrap(), area());
        assert_eq!(splitters.len(), tiles.len() - 1, "{preset:?}");
        let mut covered = 0.0;
        for (index, tile) in tiles.iter().enumerate() {
            assert!(area().contains_rect(tile.rect), "{preset:?} {tile:?}");
            for other in &tiles[index + 1..] {
                assert!(
                    tile.rect.intersect(other.rect).area() <= 0.0 + f32::EPSILON
                        || !tile.rect.intersects(other.rect),
                    "{preset:?}: {tile:?} overlaps {other:?}"
                );
            }
            // At the default window every tile is at least its panels'
            // minimum, so no body scrolls.
            let min = min_size(&DockNode::Tabs {
                panels: tile.panels.clone(),
                active: tile.active,
            });
            assert!(
                tile.rect.width() + 0.5 >= min.x && tile.rect.height() + 0.5 >= min.y,
                "{preset:?}: {tile:?} is under {min:?}"
            );
            covered += tile.rect.area();
        }
        for splitter in &splitters {
            covered += splitter.handle.area();
        }
        // Splitter handles overlap nothing but each other's ends.
        assert!(covered <= area().area() + 1.0, "{preset:?}");
        assert!(covered >= area().area() * 0.97, "{preset:?}");
    }
}

#[test]
fn split_extents_honour_minimums_then_share_in_proportion() {
    assert!((first_extent(1_008.0, 500, 100.0, 100.0) - 500.0).abs() < 0.5);
    // The ratio would starve the second child: it keeps its minimum.
    assert!((first_extent(1_008.0, 950, 100.0, 300.0) - 700.0).abs() < 0.5);
    // Too small for both minimums: shared in proportion to them.
    assert!((first_extent(308.0, 500, 100.0, 300.0) - 75.0).abs() < 0.5);
    assert!((first_extent(4.0, 500, 0.0, 0.0)).abs() < 0.5);
}

#[test]
fn drop_zones_follow_the_nearest_edge_and_the_middle_tabs_in() {
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
    assert_eq!(drop_zone(rect, pos2(200.0, 10.0)), DropZone::Center);
    assert_eq!(drop_zone(rect, pos2(200.0, 150.0)), DropZone::Center);
    assert_eq!(drop_zone(rect, pos2(20.0, 150.0)), DropZone::Left);
    assert_eq!(drop_zone(rect, pos2(390.0, 160.0)), DropZone::Right);
    assert_eq!(drop_zone(rect, pos2(200.0, 50.0)), DropZone::Top);
    assert_eq!(drop_zone(rect, pos2(180.0, 290.0)), DropZone::Bottom);
    assert_eq!(
        zone_preview(rect, DropZone::Right),
        Rect::from_min_max(pos2(200.0, 0.0), pos2(400.0, 300.0))
    );
    assert_eq!(
        zone_preview(rect, DropZone::Top),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 150.0))
    );
    assert_eq!(zone_preview(rect, DropZone::Center), rect);
}

#[test]
fn monitors_keep_the_canvas_shape_in_any_tile() {
    let header = monitors::MONITOR_HEADER_HEIGHT;
    // A wide, short tile: height-bound, 16:9.
    let (width, video) = monitor_fit(vec2(900.0, 300.0), (1_920, 1_080));
    assert!((width / video - 16.0 / 9.0).abs() < 0.02, "{width}x{video}");
    assert!(video + header <= 300.0 + 0.5);
    // A tall, narrow tile: width-bound, 16:9.
    let (width, video) = monitor_fit(vec2(320.0, 700.0), (1_920, 1_080));
    assert!((width - 320.0).abs() < 0.5);
    assert!((width / video - 16.0 / 9.0).abs() < 0.02, "{width}x{video}");
    // A 4:3 canvas keeps 4:3.
    let (width, video) = monitor_fit(vec2(900.0, 400.0), (1_440, 1_080));
    assert!((width / video - 4.0 / 3.0).abs() < 0.02, "{width}x{video}");
}

#[test]
fn dragging_a_splitter_resizes_and_a_tab_redocks() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    frame(&context, &mut shell, &state, Vec::new());
    frame(&context, &mut shell, &state, Vec::new());

    // The gap between the left column and the side tiles.
    let side = tile_rect(&shell, PanelId::Outputs);
    let before = side.width();
    let grip = pos2(side.left() - GAP / 2.0, side.center().y);
    drag(&context, &mut shell, &state, grip, grip - vec2(120.0, 0.0));
    let after = tile_rect(&shell, PanelId::Outputs).width();
    assert!(
        (after - before - 120.0).abs() < 6.0,
        "side tile {before} -> {after}"
    );
    assert!(
        shell.layout.dock.encode_tree().starts_with("h68"),
        "{}",
        shell.layout.dock.encode_tree()
    );

    // Drag the Mixer tab onto the middle of the Preview tile: they share
    // one tab group with the Mixer in front.
    let mixer = tile_rect(&shell, PanelId::Mixer);
    let preview = tile_rect(&shell, PanelId::Preview);
    drag(
        &context,
        &mut shell,
        &state,
        pos2(mixer.left() + 20.0, mixer.top() + TAB_BAR_HEIGHT / 2.0),
        preview.center(),
    );
    assert!(
        shell.layout.dock.encode_tree().contains("[preview|mixer*]"),
        "{}",
        shell.layout.dock.encode_tree()
    );
    // And the Titles tab onto the right edge of the Program tile.
    shell.layout.dock.show(PanelId::Titles);
    frame(&context, &mut shell, &state, Vec::new());
    let program = tile_rect(&shell, PanelId::Program);
    // Find the Titles tab by its painted title.
    let output = frame(&context, &mut shell, &state, Vec::new());
    let tab = find_text(&output, "TITLES").expect("the Titles tab is painted");
    drag(
        &context,
        &mut shell,
        &state,
        tab + vec2(6.0, 4.0),
        pos2(program.right() - 10.0, program.center().y),
    );
    assert!(
        shell
            .layout
            .dock
            .encode_tree()
            .contains("h500([program],[titles])"),
        "{}",
        shell.layout.dock.encode_tree()
    );
}

fn find_text(output: &egui::FullOutput, wanted: &str) -> Option<Pos2> {
    fn find(shape: &egui::Shape, wanted: &str) -> Option<Pos2> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == wanted => Some(text.pos),
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, wanted)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|clipped| find(&clipped.shape, wanted))
}

#[test]
fn tile_keys_pop_out_and_close_and_the_window_docks_back() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    frame(&context, &mut shell, &state, Vec::new());
    let mixer = tile_rect(&shell, PanelId::Mixer);

    // The pop-out key sits left of the close key at the tab bar's end.
    let close = pos2(
        mixer.right() - 4.0 - ICON_SIZE / 2.0,
        mixer.top() + TAB_BAR_HEIGHT / 2.0,
    );
    let pop = close - vec2(ICON_SIZE + 2.0, 0.0);
    frame(&context, &mut shell, &state, press(pop));
    frame(&context, &mut shell, &state, release(pop));
    assert!(shell.layout.dock.is_popped(PanelId::Mixer));
    // The window takes the tile's size.
    let popout = shell.layout.dock.popped()[0];
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let size = (mixer.width() as u16, mixer.height() as u16);
    assert_eq!(popout.geometry.map(|g| g.size), Some(size));
    // Popped out, the panel draws in its (here embedded) window.
    let output = frame(&context, &mut shell, &state, Vec::new());
    let text = painted_text(&output);
    assert!(
        text.iter()
            .any(|line| line == "Audio mixer — FreeMix Studio"),
        "{text:?}"
    );

    // The pop-out persists with its place and comes back as it was.
    let entries = shell.layout().preference_entries();
    let popouts = entries
        .iter()
        .find(|(key, _)| *key == "layout.popouts")
        .map(|(_, value)| value.clone())
        .unwrap();
    assert_eq!(popouts, format!("mixer:{}x{}", size.0, size.1));
    let mut restored = ConsoleLayout::default();
    for (key, value) in &entries {
        assert!(restored.apply_preference(key, value), "{key} = {value}");
    }
    assert_eq!(&restored, shell.layout());

    // Dock back returns it home.
    shell.layout.dock.dock_back(PanelId::Mixer);
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.layout.dock, DockLayout::default());

    // The close key hides the panel; View › Panels brings it back.
    let close = pos2(
        tile_rect(&shell, PanelId::Mixer).right() - 4.0 - ICON_SIZE / 2.0,
        mixer.top() + TAB_BAR_HEIGHT / 2.0,
    );
    frame(&context, &mut shell, &state, press(close));
    frame(&context, &mut shell, &state, release(close));
    assert!(!shell.layout.dock.is_visible(PanelId::Mixer));
    shell.layout.dock.show(PanelId::Mixer);
    assert_eq!(shell.layout.dock, DockLayout::default());
}

#[test]
fn an_unreadable_saved_layout_falls_back_to_default_with_a_notice() {
    let mut layout = ConsoleLayout::default();
    assert!(layout.apply_preference("layout.dock", "h500([preview],[preview])"));
    assert!(layout.rejected);
    assert_eq!(layout.dock, DockLayout::default());
    let mut shell = StudioShell::default();
    shell.set_layout(layout);
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    frame(&context, &mut shell, &state, Vec::new());
    // Toasts show from their second frame (egui sizes new areas first).
    let output = frame(&context, &mut shell, &state, Vec::new());
    let notice = Msg::LayoutRejected.en();
    assert!(painted_text(&output).iter().any(|line| line == notice));
    // Announced once; the flag is never written.
    assert!(!shell.layout().rejected);
    assert!(
        shell
            .layout()
            .preference_entries()
            .iter()
            .all(|(_, value)| !value.contains("rejected"))
    );
}

#[test]
fn presets_saved_layouts_and_reset() {
    let mut shell = StudioShell::default();
    shell.apply_preset(LayoutPreset::AudioFocus);
    assert_eq!(shell.layout.dock, LayoutPreset::AudioFocus.layout());
    shell.layout.saved_layouts.push(NamedLayout {
        name: "Audio".into(),
        layout: shell.layout.dock.clone(),
    });
    shell.apply_preset(LayoutPreset::Compact);
    shell.layout.set_tool_popped(ToolWindow::Color, true);
    shell.apply_saved_layout(0);
    assert_eq!(shell.layout.dock, LayoutPreset::AudioFocus.layout());
    assert!(!shell.layout.is_default_arrangement());
    let context = egui::Context::default();
    shell.reset_layout(&context);
    assert!(shell.layout.is_default_arrangement());
    assert_eq!(shell.layout.saved_layouts.len(), 1);
    assert_eq!(shell.layout.tool_popped(ToolWindow::Color), None);
    shell.delete_saved_layout(0);
    assert_eq!(shell.layout.saved_layouts, Vec::new());
    assert_eq!(shell.layout, ConsoleLayout::default());
}

#[test]
fn every_panel_draws_in_a_narrow_tile_and_in_its_own_window() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    // A cramped window: bodies scroll instead of overlapping.
    frame_at(&context, &mut shell, &state, vec2(960.0, 600.0), Vec::new());
    frame_at(&context, &mut shell, &state, vec2(960.0, 600.0), Vec::new());
    for panel in PanelId::ALL {
        shell.layout.dock.pop_out(panel, None);
    }
    assert_eq!(shell.layout.dock.root(), None);
    frame(&context, &mut shell, &state, Vec::new());
    let output = frame(&context, &mut shell, &state, Vec::new());
    let text = painted_text(&output);
    assert!(
        text.iter().any(|line| line == Msg::DockEmpty.en()),
        "{text:?}"
    );
    for panel in PanelId::ALL {
        let title = format!("{} — FreeMix Studio", Msg::en(panel.title()));
        assert!(text.contains(&title), "{title}");
    }
}

/// Renders the shell offscreen; returns the RGBA8 image.
fn render(
    shell: &mut StudioShell,
    state: &StudioUiState,
    size: Vec2,
    events: &[Vec<Event>],
) -> (usize, usize, Vec<u8>) {
    let context = egui::Context::default();
    let mut textures = Textures::default();
    let mut last = None;
    // The first frame lays the widgets out; the events follow, then one
    // more frame paints their outcome.
    for step in 0..events.len() + 2 {
        let events = step
            .checked_sub(1)
            .and_then(|index| events.get(index))
            .cloned()
            .unwrap_or_default();
        let (_, full) = frame_at(&context, shell, state, size, events);
        textures.apply(&full);
        last = Some(full);
    }
    let full = last.unwrap();
    let primitives = context.tessellate(full.shapes, full.pixels_per_point);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut canvas = Canvas::new(size.x as usize, size.y as usize, graphite_color());
    canvas.paint(&primitives, &textures, full.pixels_per_point);
    canvas.crop_rgba8(Rect::from_min_size(Pos2::ZERO, size))
}

#[test]
fn the_default_layout_renders_its_tiles_offscreen() {
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    let (width, height, rgba) = render(&mut shell, &state, SCREEN, &[]);
    assert_eq!((width, height), (1_600, 940));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pixel = |pos: Pos2| {
        let (x, y) = (pos.x as usize, pos.y as usize);
        let index = (y * width + x) * 4;
        Color32::from_rgb(rgba[index], rgba[index + 1], rgba[index + 2])
    };
    // Every tile of the default layout sits where the layout puts it:
    // each tab bar's empty end is tile coloured, each gap graphite.
    let expected = [
        PanelId::Preview,
        PanelId::Transitions,
        PanelId::Program,
        PanelId::InputBank,
        PanelId::Mixer,
        PanelId::Outputs,
        PanelId::Overlays,
    ];
    assert_eq!(
        shell.dock_ui.tile_rects.keys().copied().collect::<Vec<_>>(),
        {
            let mut sorted = expected.to_vec();
            sorted.sort();
            sorted
        }
    );
    for panel in expected {
        let rect = tile_rect(&shell, panel);
        assert!(rect.width() > 100.0 && rect.height() > 100.0, "{panel:?}");
        let tab_end = pos2(rect.right() - 2.0 * ICON_SIZE - 16.0, rect.top() + 3.0);
        assert_eq!(pixel(tab_end), panel_color(), "{panel:?} tab bar");
        let gap = pos2(rect.right() + GAP / 2.0, rect.center().y);
        if gap.x < SCREEN.x - 10.0 {
            assert_eq!(pixel(gap), graphite_color(), "{panel:?} right gap");
        }
    }
    // The Preview and Program wells are side by side at one height.
    let preview = tile_rect(&shell, PanelId::Preview);
    let program = tile_rect(&shell, PanelId::Program);
    assert!((preview.top() - program.top()).abs() < 0.5);
    assert!(preview.right() < program.left());
}
