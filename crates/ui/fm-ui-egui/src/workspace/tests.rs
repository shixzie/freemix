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

/// Every painted text with its rect and whether it was elided.
fn painted_galleys(output: &egui::FullOutput) -> Vec<(String, Rect, bool)> {
    fn collect(shape: &egui::Shape, out: &mut Vec<(String, Rect, bool)>) {
        match shape {
            egui::Shape::Text(text) => out.push((
                text.galley.text().to_owned(),
                Rect::from_min_size(text.pos, text.galley.size()),
                text.galley.elided,
            )),
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

#[test]
fn the_default_layout_is_the_classic_console_at_1080p_and_720p() {
    let state = crate::hover_audit::bank_fixture();
    for size in [vec2(1_920.0, 1_080.0), vec2(1_280.0, 720.0)] {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        frame_at(&context, &mut shell, &state, size, Vec::new());
        let (_, output) = frame_at(&context, &mut shell, &state, size, Vec::new());
        let rect = |panel| tile_rect(&shell, panel);
        let (preview, transitions, program) = (
            rect(PanelId::Preview),
            rect(PanelId::Transitions),
            rect(PanelId::Program),
        );
        let (bank, mixer, outputs, side) = (
            rect(PanelId::InputBank),
            rect(PanelId::Mixer),
            rect(PanelId::Outputs),
            rect(PanelId::Overlays),
        );
        // Preview | Transitions | Program, then Recording & streams, along
        // the top; the bank, mixer, and side tabs along the bottom.
        assert!(preview.right() < transitions.left(), "{size:?}");
        assert!(transitions.right() < program.left(), "{size:?}");
        assert!(program.right() < outputs.left(), "{size:?}");
        assert!(bank.right() < mixer.left() && mixer.right() < side.left());
        // Each row's tab bars line up across the console.
        for tile in [transitions, program, outputs] {
            assert!(
                (tile.top() - preview.top()).abs() < 0.5,
                "{size:?} {tile:?}"
            );
        }
        for tile in [mixer, side] {
            assert!((tile.top() - bank.top()).abs() < 0.5, "{size:?} {tile:?}");
        }
        // The right column lines up with the console's right edge above
        // and below.
        assert!((outputs.left() - side.left()).abs() < 0.5);
        // Preview and Program are the same width.
        assert!(
            (preview.width() - program.width()).abs() <= 12.0,
            "{size:?}: {preview:?} vs {program:?}"
        );
        // The transition column fits its tile: no wider than its widest.
        let room = transitions.width() - 2.0 * TILE_PADDING - transitions::SCROLL_GUTTER;
        let column = transitions::column_width(room);
        assert!(column <= room + 0.5, "{size:?}: {column} in {room}");
        assert!(column <= transitions::TRANSITION_COLUMN_MAX_WIDTH);
        if size.x > 1_900.0 {
            // At 1080p every tile holds its content without scrolling.
            for panel in [PanelId::Preview, PanelId::Transitions, PanelId::InputBank] {
                let (min_width, min_height) = panel.min_size();
                let tile = rect(panel);
                assert!(
                    tile.width() >= min_width + 2.0 * TILE_PADDING
                        && tile.height() >= min_height + TAB_BAR_HEIGHT + 2.0 * TILE_PADDING,
                    "{panel:?} {tile:?}"
                );
            }
            // The transition column's last line, Fade to Black's status,
            // ends inside the tile.
            let (_, status, _) = painted_galleys(&output)
                .into_iter()
                .find(|(text, _, _)| text == "PROGRAM LIVE")
                .expect("the Fade to Black status line");
            assert!(
                status.bottom() + 4.0 <= transitions.bottom() - TILE_PADDING,
                "{status:?} in {transitions:?}"
            );
        }
    }
}

#[test]
fn monitors_are_centred_both_ways_in_their_tile() {
    let header = monitors::MONITOR_HEADER_HEIGHT;
    // A tall tile: the well sits in the middle, top and bottom bars equal.
    let size = vec2(540.0, 430.0);
    let (width, video) = monitor_fit(size, (1_920, 1_080));
    let (left, top) = monitor_offset(size, (width, video));
    assert!(left.abs() < 0.5, "{left}");
    let bottom = size.y - top - header - video;
    assert!((top - bottom).abs() <= 1.0, "{top} vs {bottom}");
    assert!(top > 20.0);
    // A wide tile: pillarboxed, centred across.
    let size = vec2(900.0, 300.0);
    let (width, video) = monitor_fit(size, (1_920, 1_080));
    let (left, top) = monitor_offset(size, (width, video));
    assert!(top.abs() <= 1.0);
    assert!((left - (size.x - width) / 2.0).abs() <= 1.0);
}

#[test]
fn the_transition_column_follows_its_tile_within_its_range() {
    use transitions::{
        ColumnMetrics, TRANSITION_COLUMN_MAX_WIDTH, TRANSITION_COLUMN_MIN_WIDTH, column_width,
    };
    assert!((column_width(100.0) - TRANSITION_COLUMN_MIN_WIDTH).abs() < f32::EPSILON);
    assert!((column_width(300.5) - 300.0).abs() < f32::EPSILON);
    assert!((column_width(900.0) - TRANSITION_COLUMN_MAX_WIDTH).abs() < f32::EPSILON);
    // A tile at its minimum holds the narrowest column and the gutter its
    // scroll bar floats in.
    assert!(
        (PanelId::Transitions.min_size().0
            - (TRANSITION_COLUMN_MIN_WIDTH + transitions::SCROLL_GUTTER))
            .abs()
            < f32::EPSILON
    );
    for width in [
        TRANSITION_COLUMN_MIN_WIDTH,
        252.0,
        300.0,
        TRANSITION_COLUMN_MAX_WIDTH,
    ] {
        let metrics = ColumnMetrics::new(width);
        // Two keys, their gap, and the T-bar fill the column.
        assert!(
            2.0 * metrics.button.x + 6.0 + 56.0 <= width + 0.5,
            "{metrics:?}"
        );
        assert!(
            2.0 * metrics.button.x + 6.0 + 56.0 >= width - 2.0,
            "{metrics:?}"
        );
        // Four favourites and their three gaps fill it too.
        assert!(
            4.0 * metrics.favorite.x + 12.0 <= width + 0.5,
            "{metrics:?}"
        );
        assert!(
            4.0 * metrics.favorite.x + 12.0 >= width - 4.0,
            "{metrics:?}"
        );
        // Keys widen, never grow taller.
        assert!((metrics.button.y - 36.0).abs() < f32::EPSILON);
    }
}

/// Regression (round 7): at 1280x720 the meter key was cut to `RMS ·
/// PEAK HOL…`. The key shows whole right of the mixer's title, or not at
/// all where it does not fit (its words stay in the strips' help).
#[test]
fn the_mixer_key_shows_whole_or_not_at_all_and_never_under_its_title() {
    let state = crate::hover_audit::bank_fixture();
    for (size, shown) in [
        (vec2(1_920.0, 1_080.0), true),
        (vec2(1_280.0, 720.0), false),
    ] {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        frame_at(&context, &mut shell, &state, size, Vec::new());
        let (_, output) = frame_at(&context, &mut shell, &state, size, Vec::new());
        let texts = painted_galleys(&output);
        let title = texts
            .iter()
            .filter(|(text, _, _)| text == "AUDIO MIXER")
            .map(|(_, rect, _)| *rect)
            .max_by(|a, b| a.top().total_cmp(&b.top()))
            .expect("the mixer's section title");
        assert!(
            !texts.iter().any(|(text, _, elided)| *elided
                && text.starts_with("RMS")
                && Msg::MeterLegend
                    .en()
                    .starts_with(text.trim_end_matches('…'))),
            "{size:?}: the meter key is cut"
        );
        let legend = texts
            .iter()
            .find(|(text, _, _)| text == Msg::MeterLegend.en());
        assert_eq!(legend.is_some(), shown, "{size:?}");
        if let Some((_, legend, elided)) = legend {
            assert!(!*elided);
            assert!(legend.left() >= title.right(), "{legend:?} over {title:?}");
        }
    }
}

fn pointer_click(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn escape() -> Vec<Event> {
    vec![Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]
}

fn double_click(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    pos: Pos2,
) {
    // Past egui's triple-click window since any earlier click (frames
    // advance 1/60 s), so these two clicks are a double-click of their own.
    for _ in 0..48 {
        frame(context, shell, state, vec![Event::PointerMoved(pos)]);
    }
    for _ in 0..2 {
        frame(context, shell, state, vec![pointer_click(pos, true)]);
        frame(context, shell, state, vec![pointer_click(pos, false)]);
    }
    frame(context, shell, state, Vec::new());
}

#[test]
fn double_clicking_a_tab_maximises_its_tile_and_esc_restores_it() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    frame(&context, &mut shell, &state, Vec::new());
    let output = frame(&context, &mut shell, &state, Vec::new());
    let tab = find_text(&output, "AUDIO MIXER").expect("the mixer tab") + vec2(6.0, 4.0);
    double_click(&context, &mut shell, &state, tab);
    assert_eq!(shell.dock_ui.maximized, Some(PanelId::Mixer));
    // Only the mixer's tile is laid out, over the whole workspace.
    assert_eq!(
        shell.dock_ui.tile_rects.keys().copied().collect::<Vec<_>>(),
        vec![PanelId::Mixer]
    );
    assert!(tile_rect(&shell, PanelId::Mixer).width() > SCREEN.x - 40.0);
    // The layout itself is untouched, so it is still the default.
    assert_eq!(shell.layout.dock, DockLayout::default());
    // Its tab bar offers Restore.
    let output = frame(&context, &mut shell, &state, Vec::new());
    assert!(
        context
            .read_response(Id::new("studio-dock-restore").with("mixer"))
            .is_some()
    );
    drop(output);

    // Esc restores every tile.
    frame(&context, &mut shell, &state, escape());
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.dock_ui.maximized, None);
    assert_eq!(shell.dock_ui.tile_rects.len(), 7);

    // So does a second double-click.
    double_click(&context, &mut shell, &state, tab);
    assert_eq!(shell.dock_ui.maximized, Some(PanelId::Mixer));
    let output = frame(&context, &mut shell, &state, Vec::new());
    let tab = find_text(&output, "AUDIO MIXER").expect("the mixer tab") + vec2(6.0, 4.0);
    double_click(&context, &mut shell, &state, tab);
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.dock_ui.maximized, None);
}

#[test]
fn esc_cancels_a_manual_transition_before_it_restores_tiles() {
    let context = egui::Context::default();
    let mut state = crate::hover_audit::bank_fixture();
    if let Some(view) = &mut state.view {
        let active =
            fm_ui_model::ManualTransitionStatus::Active(fm_ui_model::ActiveManualTransition {
                kind: fm_protocol::ManualTransitionKind::Fade,
                from: view.inputs[0],
                to: view.inputs[1],
                interval_start: fm_protocol::ManualTransitionPosition::START,
                position: fm_protocol::ManualTransitionPosition::new(5_000).unwrap(),
            });
        view.switcher.desired_manual_transition = active;
        view.switcher.realized_manual_transition = active;
    }
    let mut shell = StudioShell::default();
    shell.dock_ui.maximized = Some(PanelId::Program);
    frame(&context, &mut shell, &state, Vec::new());
    let (intents, _) = frame_at(&context, &mut shell, &state, SCREEN, escape());
    assert!(
        intents.contains(&StudioIntent::CancelManualTransition),
        "{intents:?}"
    );
    assert_eq!(shell.dock_ui.maximized, Some(PanelId::Program));
}

#[test]
fn dragging_a_tile_grip_moves_its_whole_tab_group() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    shell.layout.dock.activate(PanelId::Titles);
    frame(&context, &mut shell, &state, Vec::new());
    frame(&context, &mut shell, &state, Vec::new());
    let grip = context
        .read_response(Id::new("studio-dock-grip").with("titles"))
        .expect("the side tile's grip")
        .rect
        .center();
    let bank = tile_rect(&shell, PanelId::InputBank);
    drag(
        &context,
        &mut shell,
        &state,
        grip,
        pos2(bank.left() + 12.0, bank.center().y),
    );
    assert!(
        shell
            .layout
            .dock
            .encode_tree()
            .contains("h500([overlays|stingers|titles*|playlist|media|cues],[inputs])"),
        "{}",
        shell.layout.dock.encode_tree()
    );
    // Dropped back onto itself, nothing changes.
    let before = shell.layout.dock.clone();
    let grip = context
        .read_response(Id::new("studio-dock-grip").with("titles"))
        .expect("the moved tile's grip")
        .rect
        .center();
    let side = tile_rect(&shell, PanelId::Titles);
    drag(&context, &mut shell, &state, grip, side.center());
    assert_eq!(shell.layout.dock, before);
}

#[test]
fn a_popped_window_grip_dragged_over_the_console_docks_it_there() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    frame(&context, &mut shell, &state, Vec::new());
    shell.layout.dock.pop_out(PanelId::Mixer, None);
    frame(&context, &mut shell, &state, Vec::new());
    frame(&context, &mut shell, &state, Vec::new());
    // Here the window is embedded, so it shares the console's points.
    let grip = context
        .read_response(Id::new("studio-popout-grip").with("mixer"))
        .expect("the popped window's grip")
        .rect
        .center();
    let program = tile_rect(&shell, PanelId::Program);
    let target = pos2(program.center().x, program.bottom() - 12.0);
    frame(
        &context,
        &mut shell,
        &state,
        vec![Event::PointerMoved(grip)],
    );
    frame(&context, &mut shell, &state, press(grip));
    for step in 1..=4 {
        #[allow(clippy::cast_precision_loss)]
        let at = grip + (target - grip) * (step as f32 / 4.0);
        frame(&context, &mut shell, &state, vec![Event::PointerMoved(at)]);
    }
    // While held, the console previews the drop below the Program monitor.
    assert_eq!(
        shell.dock_ui.remote_drop,
        Some(RemoteDrop {
            panel: PanelId::Mixer,
            target: PanelId::Program,
            zone: DropZone::Bottom,
        })
    );
    frame(&context, &mut shell, &state, release(target));
    frame(&context, &mut shell, &state, Vec::new());
    assert!(!shell.layout.dock.is_popped(PanelId::Mixer));
    assert!(
        shell
            .layout
            .dock
            .encode_tree()
            .contains("v500([program],[mixer])"),
        "{}",
        shell.layout.dock.encode_tree()
    );
    assert_eq!(shell.dock_ui.remote_drop, None);
}

#[test]
fn popped_window_points_map_onto_the_console_through_the_screen() {
    let pos = pos2(10.0, 20.0);
    assert_eq!(to_main_window(pos, true, None, None), Some(pos));
    assert_eq!(
        to_main_window(
            pos,
            false,
            Some(pos2(1_200.0, 500.0)),
            Some(pos2(0.0, 30.0))
        ),
        Some(pos2(1_210.0, 490.0))
    );
    // Without both windows' places (Wayland) there is no mapping; Dock
    // back to… still lists every place.
    assert_eq!(to_main_window(pos, false, None, Some(Pos2::ZERO)), None);
    assert_eq!(to_main_window(pos, false, Some(Pos2::ZERO), None), None);
}

#[test]
fn no_op_drops_are_not_previewed() {
    let tile = Tile {
        panels: vec![PanelId::Overlays, PanelId::Titles],
        active: 0,
        rect: Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0)),
    };
    assert!(drop_is_noop(
        DragSource::Group(PanelId::Titles),
        &tile,
        DropZone::Left
    ));
    assert!(drop_is_noop(
        DragSource::Tab(PanelId::Titles),
        &tile,
        DropZone::Center
    ));
    assert!(!drop_is_noop(
        DragSource::Tab(PanelId::Titles),
        &tile,
        DropZone::Left
    ));
    assert!(!drop_is_noop(
        DragSource::Tab(PanelId::Mixer),
        &tile,
        DropZone::Center
    ));
    let lone = Tile {
        panels: vec![PanelId::Mixer],
        ..tile
    };
    assert!(drop_is_noop(
        DragSource::Tab(PanelId::Mixer),
        &lone,
        DropZone::Right
    ));
}

#[test]
#[ignore = "writes the offscreen renders to $FM_DOCK_RENDER_DIR for review"]
fn render_the_default_layout_for_review() {
    let Ok(dir) = std::env::var("FM_DOCK_RENDER_DIR") else {
        return;
    };
    let state = crate::hover_audit::bank_fixture();
    for size in [vec2(1_920.0, 1_080.0), vec2(1_280.0, 720.0)] {
        let mut shell = StudioShell::default();
        let (width, height, rgba) = render(&mut shell, &state, size, &[]);
        std::fs::write(
            format!("{dir}/default-{width}x{height}.png"),
            crate::offscreen::encode_png(width, height, &rgba),
        )
        .unwrap();
    }
}

/// The Cues panel maximised on the audit show (RC-017): the standby cue's
/// "GO will:" preview, and the same preview as the hover of a cue's state
/// chip.
#[test]
#[ignore = "writes the Cues panel renders to $FM_CUE_RENDER_DIR for review"]
fn render_the_cue_preview_for_review() {
    let Ok(dir) = std::env::var("FM_CUE_RENDER_DIR") else {
        return;
    };
    let state = crate::hover_audit::bank_fixture();
    let size = vec2(760.0, 1_000.0);
    let shell = || {
        let mut shell = StudioShell::default();
        shell.layout.dock.show(PanelId::Cues);
        shell.cues.select(1, None);
        shell.dock_ui.maximized = Some(PanelId::Cues);
        shell
    };
    let (width, height, rgba) = render(&mut shell(), &state, size, &[]);
    std::fs::write(
        format!("{dir}/studio-cue-preview.png"),
        crate::offscreen::encode_png(width, height, &rgba),
    )
    .unwrap();
    // Hover the Opener's LIVE chip (the lowest painted "LIVE": the top
    // bar has one too) until its tooltip shows.
    let context = egui::Context::default();
    let mut textures = Textures::default();
    let mut shell = shell();
    let (_, output) = frame_at(&context, &mut shell, &state, size, Vec::new());
    textures.apply(&output);
    context.all_styles_mut(|style| {
        style.interaction.tooltip_delay = 0.0;
        style.interaction.show_tooltips_only_when_still = false;
    });
    let (_, output) = frame_at(&context, &mut shell, &state, size, Vec::new());
    textures.apply(&output);
    let chip = chip_rect(&output, "LIVE").expect("the Opener's LIVE chip is painted");
    let mut last = None;
    for _ in 0..4 {
        let hover = vec![Event::PointerMoved(chip.center())];
        let (_, output) = frame_at(&context, &mut shell, &state, size, hover);
        textures.apply(&output);
        last = Some(output);
    }
    let full = last.unwrap();
    let primitives = context.tessellate(full.shapes, full.pixels_per_point);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut canvas = Canvas::new(size.x as usize, size.y as usize, graphite_color());
    canvas.paint(&primitives, &textures, full.pixels_per_point);
    let (width, height, rgba) = canvas.crop_rgba8(Rect::from_min_size(Pos2::ZERO, size));
    std::fs::write(
        format!("{dir}/studio-cue-preview-hover.png"),
        crate::offscreen::encode_png(width, height, &rgba),
    )
    .unwrap();
}

/// The lowest rectangle of the painted text `text`, if one is painted.
fn chip_rect(output: &egui::FullOutput, text: &str) -> Option<Rect> {
    fn find(shape: &egui::Shape, text: &str, out: &mut Vec<Rect>) {
        match shape {
            egui::Shape::Text(shape) if shape.galley.text() == text => {
                out.push(shape.visual_bounding_rect());
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    find(shape, text, out);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    for clipped in &output.shapes {
        find(&clipped.shape, text, &mut found);
    }
    found.into_iter().max_by(|a, b| a.min.y.total_cmp(&b.min.y))
}

/// Regression: at a 760-point window the padlock's UNLOCKED ran under
/// the REC badge and read "UN". The top bar sheds detail instead: no two
/// of its words overlap, none is cut by a clip, and none leaves the
/// window, in English and Spanish, from wide to narrow.
#[test]
fn the_top_bar_never_overlaps_or_clips_its_words() {
    fn collect(shape: &egui::Shape, clip: Rect, out: &mut Vec<(String, Rect, Rect)>) {
        match shape {
            egui::Shape::Text(text) => {
                out.push((
                    text.galley.text().to_owned(),
                    text.visual_bounding_rect(),
                    clip,
                ));
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, clip, out);
                }
            }
            _ => {}
        }
    }
    let state = crate::hover_audit::bank_fixture();
    for language in [
        fm_ui_model::i18n::Language::English,
        fm_ui_model::i18n::Language::Spanish,
    ] {
        for width in [1_920.0, 1_280.0, 1_000.0, 760.0, 640.0, 560.0] {
            let context = egui::Context::default();
            let mut shell = StudioShell::default();
            shell.set_locale(crate::l10n::Locale::new(language));
            let size = vec2(width, 700.0);
            frame_at(&context, &mut shell, &state, size, Vec::new());
            let (_, output) = frame_at(&context, &mut shell, &state, size, Vec::new());
            let mut words = Vec::new();
            for clipped in &output.shapes {
                collect(&clipped.shape, clipped.clip_rect, &mut words);
            }
            // The top bar sits under the 26-point menu bar.
            words.retain(|(_, rect, _)| rect.center().y > 28.0 && rect.center().y < 70.0);
            assert!(
                words.iter().any(|(text, ..)| text == "REC"),
                "{language:?} {width}: {words:?}"
            );
            for (index, (text, rect, clip)) in words.iter().enumerate() {
                assert!(
                    clip.expand(0.5).contains_rect(*rect) && rect.right() <= width,
                    "{language:?} {width}: {text:?} is cut ({rect:?} in {clip:?})"
                );
                for (other, other_rect, _) in &words[index + 1..] {
                    assert!(
                        !rect.shrink(0.5).intersects(other_rect.shrink(0.5)),
                        "{language:?} {width}: {text:?} overlaps {other:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn tiny_windows_and_maximised_tiles_draw_without_panicking() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    for size in [vec2(1.0, 1.0), vec2(40.0, 30.0), vec2(200.0, 120.0)] {
        frame_at(&context, &mut shell, &state, size, Vec::new());
        shell.dock_ui.maximized = Some(PanelId::Overlays);
        frame_at(&context, &mut shell, &state, size, Vec::new());
        shell.dock_ui.maximized = None;
    }
}

/// Maximises `panel`'s tile and draws until the workspace has seen it.
fn maximise(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    panel: PanelId,
) {
    frame(context, shell, state, Vec::new());
    shell.apply_dock_action(context, DockAction::ToggleMaximized(panel));
    frame(context, shell, state, Vec::new());
    frame(context, shell, state, Vec::new());
    assert_eq!(shell.dock_ui.maximized, Some(panel));
}

#[test]
fn closing_or_popping_out_the_maximised_tab_keeps_its_tile_maximised() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    for action in [DockAction::Close, DockAction::PopOut] {
        let mut shell = StudioShell::default();
        maximise(&context, &mut shell, &state, PanelId::Overlays);
        shell.apply_dock_action(&context, action(PanelId::Overlays));
        frame(&context, &mut shell, &state, Vec::new());
        // The rest of the group stays maximised.
        assert_eq!(shell.dock_ui.maximized, Some(PanelId::Stingers));
        assert_eq!(
            shell.dock_ui.tile_rects.keys().copied().collect::<Vec<_>>(),
            vec![PanelId::Stingers]
        );
    }
    // Closing a lone maximised tile restores the others.
    let mut shell = StudioShell::default();
    maximise(&context, &mut shell, &state, PanelId::Mixer);
    shell.apply_dock_action(&context, DockAction::Close(PanelId::Mixer));
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.dock_ui.maximized, None);
    assert_eq!(shell.dock_ui.tile_rects.len(), 6);
}

#[test]
fn a_panel_docked_beside_a_maximised_tile_restores_the_tiles() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    // Docked back from its window, beside the maximised tile.
    let mut shell = StudioShell::default();
    shell.layout.dock.pop_out(PanelId::Program, None);
    maximise(&context, &mut shell, &state, PanelId::Mixer);
    shell
        .layout
        .dock
        .move_panel(PanelId::Program, PanelId::Mixer, DropZone::Left);
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.dock_ui.maximized, None);
    assert!(shell.dock_ui.tile_rects.contains_key(&PanelId::Program));
    // Reopened from View › Panels.
    let mut shell = StudioShell::default();
    shell.layout.dock.close(PanelId::Program);
    maximise(&context, &mut shell, &state, PanelId::Mixer);
    shell.layout.dock.show(PanelId::Program);
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.dock_ui.maximized, None);
    // A tab dragged out of the maximised group to beside it.
    let mut shell = StudioShell::default();
    maximise(&context, &mut shell, &state, PanelId::Overlays);
    shell.apply_dock_action(
        &context,
        DockAction::Move(PanelId::Titles, PanelId::Overlays, DropZone::Left),
    );
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.dock_ui.maximized, None);
    assert!(shell.dock_ui.tile_rects.contains_key(&PanelId::Titles));
    // Tabbed into the maximised group, it shows there.
    let mut shell = StudioShell::default();
    shell.layout.dock.pop_out(PanelId::Program, None);
    maximise(&context, &mut shell, &state, PanelId::Mixer);
    shell
        .layout
        .dock
        .move_panel(PanelId::Program, PanelId::Mixer, DropZone::Center);
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.dock_ui.maximized, Some(PanelId::Mixer));
}

#[test]
fn applying_or_resetting_a_layout_restores_maximised_tiles() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    shell.layout.saved_layouts.push(NamedLayout {
        name: "Compact".into(),
        layout: LayoutPreset::Compact.layout(),
    });
    let applies: [fn(&mut StudioShell, &egui::Context); 3] = [
        |shell, _| shell.apply_preset(LayoutPreset::AudioFocus),
        |shell, _| shell.apply_saved_layout(0),
        |shell, context| shell.reset_layout(context),
    ];
    for apply in applies {
        maximise(&context, &mut shell, &state, PanelId::Mixer);
        apply(&mut shell, &context);
        frame(&context, &mut shell, &state, Vec::new());
        assert_eq!(shell.dock_ui.maximized, None);
        assert!(shell.dock_ui.tile_rects.len() > 1);
    }
}

/// What a native window with `info` reports.
fn window_outcome_for(context: &egui::Context, info: egui::ViewportInfo) -> WindowOutcome {
    let mut raw = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        ..RawInput::default()
    };
    raw.viewports.insert(ViewportId::ROOT, info);
    context.begin_pass(raw);
    let ui = Ui::new(
        context.clone(),
        Id::new("fm-dock-test"),
        UiBuilder::new().layer_id(LayerId::background()),
    );
    let outcome = window_outcome(&ui, ViewportClass::Immediate, false);
    drop(ui);
    let _ = context.end_pass();
    outcome
}

#[test]
fn a_minimised_window_keeps_its_last_place() {
    let context = egui::Context::default();
    let outcome = |info| window_outcome_for(&context, info);
    let placed = egui::ViewportInfo {
        inner_rect: Some(Rect::from_min_size(pos2(100.0, 130.0), vec2(640.0, 400.0))),
        outer_rect: Some(Rect::from_min_size(pos2(100.0, 100.0), vec2(640.0, 430.0))),
        minimized: Some(false),
        ..egui::ViewportInfo::default()
    };
    assert_eq!(
        outcome(placed).geometry,
        Some(WindowGeometry {
            size: (640, 400),
            position: Some((100, 100)),
            maximized: false,
        })
    );
    // Windows parks a minimised window at -32000, -32000.
    let minimised = egui::ViewportInfo {
        inner_rect: Some(Rect::from_min_size(
            pos2(-32_000.0, -32_000.0),
            vec2(160.0, 0.0),
        )),
        outer_rect: Some(Rect::from_min_size(
            pos2(-32_000.0, -32_000.0),
            vec2(160.0, 28.0),
        )),
        minimized: Some(true),
        ..egui::ViewportInfo::default()
    };
    assert_eq!(outcome(minimised).geometry, None);
}

#[test]
fn enter_right_after_typing_saves_the_name_as_typed() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    shell.open_layout_name_dialog(None);
    frame(&context, &mut shell, &state, Vec::new());
    frame(&context, &mut shell, &state, Vec::new());
    frame(
        &context,
        &mut shell,
        &state,
        vec![Event::Text("Live".into())],
    );
    // The last key and Enter arrive in one frame.
    frame(
        &context,
        &mut shell,
        &state,
        vec![
            Event::Text("2".into()),
            Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let names: Vec<_> = shell
        .layout
        .saved_layouts
        .iter()
        .map(|saved| saved.name.as_str())
        .collect();
    assert_eq!(names, ["Live2"]);
    assert!(!shell.layout_dialog_open());
}

#[test]
fn saved_layout_names_differing_only_in_case_are_one_layout() {
    let mut layout = ConsoleLayout::default();
    let compact = NamedLayout {
        name: "Live".into(),
        layout: LayoutPreset::Compact.layout(),
    };
    let audio = NamedLayout {
        name: "LIVE".into(),
        layout: LayoutPreset::AudioFocus.layout(),
    };
    assert!(layout.apply_preference("layout.saved", &compact.encode()));
    assert!(layout.apply_preference("layout.saved", &audio.encode()));
    assert_eq!(layout.saved_layouts, vec![audio]);
}

#[test]
fn a_maximised_window_keeps_the_place_it_restores_to() {
    let context = egui::Context::default();
    let restored = WindowGeometry {
        size: (640, 400),
        position: Some((100, 100)),
        maximized: false,
    };
    // Maximised, the window reports the whole monitor: only the flag is
    // kept, beside the place it returns to.
    let maximised = window_outcome_for(
        &context,
        egui::ViewportInfo {
            inner_rect: Some(Rect::from_min_size(pos2(0.0, 30.0), vec2(1_920.0, 1_050.0))),
            outer_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1_920.0, 1_080.0))),
            minimized: Some(false),
            maximized: Some(true),
            ..egui::ViewportInfo::default()
        },
    );
    assert_eq!(maximised.geometry, None);
    assert_eq!(
        maximised.kept(Some(restored)),
        Some(restored.with_maximized(true))
    );
    // Never placed before, it keeps the default size.
    assert_eq!(
        maximised.kept(None),
        Some(WindowGeometry::from_points(DEFAULT_WINDOW_SIZE, None).with_maximized(true))
    );
    // Un-maximised, its place is kept again, no longer flagged.
    let normal = window_outcome_for(
        &context,
        egui::ViewportInfo {
            inner_rect: Some(Rect::from_min_size(pos2(100.0, 130.0), vec2(640.0, 400.0))),
            outer_rect: Some(Rect::from_min_size(pos2(100.0, 100.0), vec2(640.0, 430.0))),
            maximized: Some(false),
            ..egui::ViewportInfo::default()
        },
    );
    assert_eq!(
        normal.kept(Some(restored.with_maximized(true))),
        Some(restored)
    );

    // A popped panel saved maximised opens maximised at its restored size.
    let mut shell = StudioShell::default();
    let builder = shell.popout_builder(
        PopKey::Panel(PanelId::Mixer),
        "Mixer",
        Some(restored.with_maximized(true)),
        Vec2::splat(200.0),
    );
    assert_eq!(builder.maximized, Some(true));
    assert_eq!(builder.inner_size, Some(vec2(640.0, 400.0)));
    assert_eq!(builder.position, Some(pos2(100.0, 100.0)));
    let builder = shell.popout_builder(
        PopKey::Panel(PanelId::Outputs),
        "Outputs",
        Some(restored),
        Vec2::splat(200.0),
    );
    assert_eq!(builder.maximized, Some(false));
}

#[test]
fn popping_out_of_a_maximised_tile_takes_the_tiles_own_size() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    frame(&context, &mut shell, &state, Vec::new());
    let natural = tile_rect(&shell, PanelId::Mixer);
    maximise(&context, &mut shell, &state, PanelId::Mixer);
    // Maximised, the tile covers the whole area ...
    assert!(tile_rect(&shell, PanelId::Mixer).width() > natural.width() * 1.5);
    shell.apply_dock_action(&context, DockAction::PopOut(PanelId::Mixer));
    // ... but its window takes the size it has among the other tiles.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let size = (natural.width() as u16, natural.height() as u16);
    assert_eq!(
        shell.layout.dock.popped()[0].geometry.map(|g| g.size),
        Some(size)
    );
    // An inactive tab of a maximised group pops out at its group's size.
    let mut shell = StudioShell::default();
    frame(&context, &mut shell, &state, Vec::new());
    let natural = tile_rect(&shell, PanelId::Overlays);
    maximise(&context, &mut shell, &state, PanelId::Overlays);
    shell.apply_dock_action(&context, DockAction::PopOut(PanelId::Titles));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let size = (natural.width() as u16, natural.height() as u16);
    assert_eq!(
        shell.layout.dock.popped()[0].geometry.map(|g| g.size),
        Some(size)
    );
}

#[test]
fn saved_windows_open_on_a_monitor_that_is_still_there() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    shell.set_screens(vec![ScreenRect {
        x: 0,
        y: 0,
        width: 1_920,
        height: 1_080,
    }]);
    // Saved on a second monitor that has since been unplugged.
    let saved = WindowGeometry {
        size: (640, 400),
        position: Some((2_400, 200)),
        maximized: true,
    };
    shell.layout.dock.pop_out(PanelId::Mixer, Some(saved));
    let builder = shell.popout_builder(
        PopKey::Panel(PanelId::Mixer),
        "Mixer",
        Some(saved),
        Vec2::splat(200.0),
    );
    assert_eq!(builder.position, Some(pos2(640.0, 340.0)));
    assert_eq!(builder.inner_size, Some(vec2(640.0, 400.0)));
    assert_eq!(builder.maximized, Some(true));
    // The window keeps the place it was opened at while it is open.
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(
        shell.dock_ui.spawned.get(&PopKey::Panel(PanelId::Mixer)),
        Some(&Some(WindowGeometry {
            position: Some((640, 340)),
            ..saved
        }))
    );
    // A tool window saved off-screen is brought back the same way.
    let (_, builder) = shell.tool_viewport(
        ToolWindow::Color,
        Some(WindowGeometry {
            size: (2_400, 900),
            position: Some((-3_000, 50)),
            maximized: false,
        }),
    );
    assert_eq!(builder.position, Some(pos2(0.0, 90.0)));
    assert_eq!(builder.inner_size, Some(vec2(1_920.0, 900.0)));
}

#[test]
fn showing_a_popped_out_panel_brings_its_window_to_the_front() {
    let context = egui::Context::default();
    let state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    shell.layout.dock.pop_out(PanelId::Mixer, None);
    frame(&context, &mut shell, &state, Vec::new());
    shell.show_panel(PanelId::Mixer);
    assert!(shell.layout.dock.is_popped(PanelId::Mixer));
    assert!(shell.dock_ui.focus.contains(&PanelId::Mixer));
    // The window is raised on the next frame, once.
    frame(&context, &mut shell, &state, Vec::new());
    assert!(shell.dock_ui.focus.is_empty());
    // A hidden or docked panel just shows; there is no window to raise.
    shell.layout.dock.close(PanelId::Outputs);
    shell.show_panel(PanelId::Outputs);
    shell.show_panel(PanelId::Program);
    assert!(shell.layout.dock.is_docked(PanelId::Outputs));
    assert!(shell.dock_ui.focus.is_empty());
    // A raise asked for a panel docked back before the frame is dropped.
    shell.show_panel(PanelId::Mixer);
    shell.layout.dock.dock_back(PanelId::Mixer);
    frame(&context, &mut shell, &state, Vec::new());
    assert!(shell.dock_ui.focus.is_empty());
}

/// The area id of `tool`'s floating window.
fn tool_window_id(tool: ToolWindow) -> Id {
    Id::new(match tool {
        ToolWindow::Color => "studio-color-window",
        ToolWindow::Animation => "studio-animation-window",
        ToolWindow::Designer => "studio-designer-window",
    })
}

fn open_tool(shell: &mut StudioShell, tool: ToolWindow) {
    match tool {
        ToolWindow::Color => shell.open_color(None),
        ToolWindow::Animation => shell.open_animation(None),
        ToolWindow::Designer => shell.open_designer(None),
    }
}

/// Regression (round 7): the Colour and Animation windows had no title
/// bar, their close key sat mid-row after the last key, and the Animation
/// window opened over the top bar's padlock. Every floating tool window
/// opens inside the work area below the top bar, with a header whose Pop
/// out and Close keys sit side by side at its right end.
#[test]
fn floating_tool_windows_open_below_the_top_bar_with_their_keys_at_the_right() {
    let state = crate::hover_audit::bank_fixture();
    for size in [vec2(1_920.0, 1_080.0), vec2(1_280.0, 720.0)] {
        for tool in ToolWindow::ALL {
            let context = egui::Context::default();
            let mut shell = StudioShell::default();
            open_tool(&mut shell, tool);
            for _ in 0..3 {
                frame_at(&context, &mut shell, &state, size, Vec::new());
            }
            let area = shell.work_area.expect("the work area is known");
            // Below the 26-point menu bar and the 46-point top bar.
            assert!(area.top() >= 72.0, "{area:?}");
            let window = context
                .memory(|memory| memory.area_rect(tool_window_id(tool)))
                .unwrap_or_else(|| panic!("{tool:?} has no window"));
            assert!(
                window.top() >= area.top() - 0.5 && window.bottom() <= area.bottom() + 0.5,
                "{tool:?} at {size:?}: {window:?} leaves {area:?}"
            );
            let key = |name: &str| {
                context
                    .read_response(Id::new(name).with(tool.key()))
                    .unwrap_or_else(|| panic!("{tool:?} has no {name}"))
                    .rect
            };
            let (close, pop) = (key("studio-tool-close"), key("studio-tool-popout"));
            assert!(
                window.right() - close.right() <= 16.0,
                "{tool:?}: close {close:?} is not at the right of {window:?}"
            );
            assert!(close.top() - window.top() <= 12.0, "{tool:?}: {close:?}");
            assert!(pop.right() <= close.left() && close.left() - pop.right() <= 4.0);
            assert!((pop.center().y - close.center().y).abs() < 0.5);
        }
    }
}

#[test]
fn a_floating_tool_window_header_closes_and_pops_out_the_tool() {
    let state = crate::hover_audit::bank_fixture();
    let size = vec2(1_920.0, 1_080.0);
    for (name, pops) in [("studio-tool-close", false), ("studio-tool-popout", true)] {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        open_tool(&mut shell, ToolWindow::Color);
        for _ in 0..3 {
            frame_at(&context, &mut shell, &state, size, Vec::new());
        }
        let at = context
            .read_response(Id::new(name).with(ToolWindow::Color.key()))
            .unwrap()
            .rect
            .center();
        frame_at(
            &context,
            &mut shell,
            &state,
            size,
            vec![Event::PointerMoved(at)],
        );
        frame_at(&context, &mut shell, &state, size, press(at));
        frame_at(&context, &mut shell, &state, size, release(at));
        if pops {
            assert!(shell.color.open);
            assert!(shell.layout.tool_popped(ToolWindow::Color).is_some());
        } else {
            assert!(!shell.color.open);
            assert!(shell.layout.tool_popped(ToolWindow::Color).is_none());
        }
    }
}

#[test]
fn tabs_fit_whole_or_fold_into_more_tabs() {
    // All fit: every tab at its natural width, no More tabs key.
    let fit = fit_tabs(&[80.0, 80.0, 60.0], 0, 400.0, 24.0);
    assert_eq!(fit.widths, vec![Some(80.0), Some(80.0), Some(60.0)]);
    assert!(!fit.overflow);
    // Too many: the active tab always shows, the others in order while
    // they fit whole, the rest under More tabs.
    let fit = fit_tabs(&[90.0, 90.0, 70.0, 80.0, 70.0, 60.0], 5, 300.0, 24.0);
    assert!(fit.overflow);
    assert_eq!(fit.widths[5], Some(60.0));
    assert_eq!(fit.widths[0], Some(90.0));
    assert_eq!(fit.widths[1], Some(90.0));
    assert!(fit.widths[2..5].iter().all(Option::is_none), "{fit:?}");
    let shown: f32 = fit.widths.iter().flatten().map(|width| width + 2.0).sum();
    assert!(shown + 24.0 <= 300.0, "{fit:?}");
    // An active tab wider than the room is the only one narrowed.
    let fit = fit_tabs(&[400.0, 50.0], 0, 200.0, 24.0);
    assert_eq!(fit.widths[1], None);
    assert!(fit.widths[0].unwrap() <= 200.0 - 24.0);
}

/// Regression (round 7): at 1280x720 the side group's six tabs were cut
/// to `OVE…`, `STI…`, `PLA…`, `ME…`. Tabs show whole, and those with no
/// room are listed under More tabs.
#[test]
fn a_narrow_tab_group_folds_tabs_into_more_tabs_instead_of_cutting_them() {
    let state = crate::hover_audit::bank_fixture();
    let side = [
        PanelId::Overlays,
        PanelId::Stingers,
        PanelId::Titles,
        PanelId::Playlist,
        PanelId::Media,
        PanelId::Cues,
    ];
    for language in [
        fm_ui_model::i18n::Language::English,
        fm_ui_model::i18n::Language::Spanish,
    ] {
        for (size, folds) in [
            (vec2(1_920.0, 1_080.0), false),
            (vec2(1_280.0, 720.0), true),
        ] {
            let context = egui::Context::default();
            let mut shell = StudioShell::default();
            let locale = crate::l10n::Locale::new(language);
            shell.set_locale(locale);
            frame_at(&context, &mut shell, &state, size, Vec::new());
            let (_, output) = frame_at(&context, &mut shell, &state, size, Vec::new());
            let words = painted_text(&output);
            for panel in side {
                let title = locale.text(panel.title()).to_uppercase();
                assert!(
                    !words
                        .iter()
                        .any(|word| word.ends_with('…')
                            && title.starts_with(word.trim_end_matches('…'))),
                    "{language:?} {size:?}: {title} is cut in {words:?}"
                );
            }
            let more = side.iter().any(|panel| {
                context
                    .read_response(Id::new("studio-dock-more").with(panel.key()))
                    .is_some()
            });
            assert_eq!(more, folds, "{language:?} {size:?}");
        }
    }
}

/// Regression (round 7): at 1920x1080 the monitors left a bare band above
/// and below the 16:9 picture. A tall enough tile shows the bus's audio
/// levels under the picture; a short one keeps all its height for it.
#[test]
fn tall_monitor_tiles_show_audio_levels_under_the_picture() {
    let state = crate::hover_audit::bank_fixture();
    for (size, strips) in [(vec2(1_920.0, 1_080.0), 2), (vec2(1_280.0, 720.0), 0)] {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        frame_at(&context, &mut shell, &state, size, Vec::new());
        let (_, output) = frame_at(&context, &mut shell, &state, size, Vec::new());
        let marks = painted_text(&output)
            .iter()
            .filter(|word| *word == "-50")
            .count();
        assert_eq!(marks, strips, "{size:?}");
    }
    assert!(monitor_meters_fit(vec2(520.0, 440.0), 293.0));
    assert!(!monitor_meters_fit(vec2(334.0, 250.0), 188.0));
}

/// The Web access window (RC-007) asks for the gateway status when it
/// opens and again about once a second, and a click on Start pairing asks
/// to pair a device with the chosen role.
#[test]
fn the_web_access_window_polls_and_starts_pairing() {
    let mut state = crate::hover_audit::bank_fixture();
    if let Some(view) = state.view.as_mut() {
        view.web_access = Some(crate::hover_audit::web_access_answer());
    }
    let size = vec2(1_280.0, 900.0);
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    shell.web_access.open = true;
    let status = StudioIntent::WebAccess(fm_protocol::WebAccessAction::Status);
    let (intents, _) = frame_at(&context, &mut shell, &state, size, Vec::new());
    assert!(intents.contains(&status), "{intents:?}");
    // The next frames, at once, do not ask again.
    let (intents, _) = frame_at(&context, &mut shell, &state, size, Vec::new());
    assert!(!intents.contains(&status), "{intents:?}");
    let (_, output) = frame_at(&context, &mut shell, &state, size, Vec::new());
    let start = chip_rect(&output, "Start pairing").expect("Start pairing is painted");
    let click = |pressed| Event::PointerButton {
        pos: start.center(),
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let mut all = Vec::new();
    for events in [
        vec![Event::PointerMoved(start.center())],
        vec![click(true)],
        vec![click(false)],
    ] {
        let (intents, _) = frame_at(&context, &mut shell, &state, size, events);
        all.extend(intents);
    }
    assert!(
        all.contains(&StudioIntent::WebAccess(
            fm_protocol::WebAccessAction::Pair {
                role: fm_protocol::WebDeviceRole::Operator
            }
        )),
        "{all:?}"
    );
}

/// Renders the Web access window with a pairing under way for review
/// (`FM_WEB_ACCESS_RENDER_DIR=<dir> cargo test -p fm-ui-egui --lib
/// render_the_web_access_window_for_review -- --ignored`).
#[test]
#[ignore = "writes a review screenshot"]
fn render_the_web_access_window_for_review() {
    let Ok(dir) = std::env::var("FM_WEB_ACCESS_RENDER_DIR") else {
        return;
    };
    let mut state = crate::hover_audit::bank_fixture();
    if let Some(view) = state.view.as_mut() {
        view.web_access = Some(crate::hover_audit::web_access_answer());
    }
    let mut shell = StudioShell::default();
    shell.web_access.open = true;
    // Empty frames let the modal finish fading in.
    let settle = vec![Vec::new(); 30];
    let (width, height, rgba) = render(&mut shell, &state, vec2(1_280.0, 1_000.0), &settle);
    std::fs::write(
        format!("{dir}/studio-web-access.png"),
        crate::offscreen::encode_png(width, height, &rgba),
    )
    .unwrap();
}

/// Where `wanted` is painted, every time.
fn find_all_text(output: &egui::FullOutput, wanted: &str) -> Vec<Pos2> {
    fn find(shape: &egui::Shape, wanted: &str, out: &mut Vec<Pos2>) {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == wanted => out.push(text.pos),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    find(shape, wanted, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &output.shapes {
        find(&clipped.shape, wanted, &mut out);
    }
    out
}

/// RC-004: clicking an input tile selects its input on this console (a
/// click elsewhere that paints the name does not), and the selection goes
/// when the input does.
#[test]
fn clicking_an_input_tile_selects_its_input() {
    let context = egui::Context::default();
    let mut state = crate::hover_audit::bank_fixture();
    let mut shell = StudioShell::default();
    let output = frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.selected_input(), None);
    let view = state.view.as_ref().unwrap();
    let (input, name) = (view.inputs[1], view.input_names[1].clone());
    let spots = find_all_text(&output, &name);
    assert!(!spots.is_empty(), "{name} is painted");
    // Only the tile selects: try each place the name is painted.
    for at in spots {
        let at = at + vec2(4.0, 4.0);
        frame(&context, &mut shell, &state, press(at));
        frame(&context, &mut shell, &state, release(at));
        // Past any double-click window.
        frame(&context, &mut shell, &state, vec![Event::PointerGone]);
        if shell.selected_input().is_some() {
            break;
        }
    }
    assert_eq!(shell.selected_input(), Some(input));
    assert_eq!(crate::selection::selected_input(&context), Some(input));

    // The show drops the input: the selection goes with it.
    let view = state.view.as_mut().unwrap();
    view.inputs.remove(1);
    view.input_names.remove(1);
    frame(&context, &mut shell, &state, Vec::new());
    assert_eq!(shell.selected_input(), None);
}
