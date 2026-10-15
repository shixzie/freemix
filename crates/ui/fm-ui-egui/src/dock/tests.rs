use super::*;

use PanelId::{
    InputBank, Media, Mixer, Outputs, Overlays, Playlist, Preview, Program, Stingers, Titles,
    Transitions,
};

const DEFAULT_TREE: &str = "h765(v500(h375([preview],h394([transitions],[program])),\
h600([inputs],[mixer])),v500([outputs],[overlays|stingers|titles|playlist|media]))";

fn group_of(layout: &DockLayout, panel: PanelId) -> Vec<PanelId> {
    let mut layout = layout.clone();
    match layout.root.as_mut().and_then(|root| root.group_mut(panel)) {
        Some(DockNode::Tabs { panels, .. }) => panels.clone(),
        _ => Vec::new(),
    }
}

fn all_shown_once(layout: &DockLayout) {
    layout.validate().unwrap();
}

#[test]
fn default_layout_shows_every_panel_once_and_encodes_compactly() {
    let layout = DockLayout::default();
    for panel in PanelId::ALL {
        assert!(layout.is_docked(panel), "{panel:?}");
        assert!(!layout.is_popped(panel));
        assert_eq!(PanelId::from_key(panel.key()), Some(panel));
    }
    assert_eq!(layout.encode_tree(), DEFAULT_TREE);
    assert_eq!(layout.encode_popouts(), "none");
    for preset in LayoutPreset::ALL {
        let layout = preset.layout();
        all_shown_once(&layout);
        assert_eq!(layout.docked().len(), PanelId::ALL.len(), "{preset:?}");
    }
    assert_eq!(
        layout.active_tabs(),
        vec![
            Preview,
            Transitions,
            Program,
            InputBank,
            Mixer,
            Outputs,
            Overlays
        ]
    );
}

#[test]
fn split_tab_and_move_rearrange_the_tree() {
    let mut layout = DockLayout::default();
    // Tab the mixer in with the input bank: the bank's split collapses.
    assert!(layout.move_panel(Mixer, InputBank, DropZone::Center));
    assert_eq!(group_of(&layout, InputBank), vec![InputBank, Mixer]);
    assert!(layout.is_showing(Mixer));
    assert!(!layout.is_showing(InputBank));
    all_shown_once(&layout);
    assert!(
        layout
            .encode_tree()
            .contains("v500(h375([preview],h394([transitions],[program])),[inputs|mixer*])")
    );

    // Split Titles out to the left of the Program monitor.
    assert!(layout.move_panel(Titles, Program, DropZone::Left));
    assert_eq!(group_of(&layout, Titles), vec![Titles]);
    assert!(layout.encode_tree().contains("h500([titles],[program])"));
    assert_eq!(
        group_of(&layout, Overlays),
        vec![Overlays, Stingers, Playlist, Media]
    );
    all_shown_once(&layout);

    // Stack Outputs under the Preview monitor; its old split collapses
    // into the remaining side tabs.
    assert!(layout.move_panel(Outputs, Preview, DropZone::Bottom));
    assert!(layout.encode_tree().contains("v500([preview],[outputs])"));
    assert!(
        layout
            .encode_tree()
            .ends_with(",[overlays|stingers|playlist|media])")
    );
    all_shown_once(&layout);

    // Dropping a lone panel on itself, or onto its own group's centre,
    // changes nothing.
    let before = layout.clone();
    assert!(!layout.move_panel(Preview, Preview, DropZone::Left));
    assert!(!layout.move_panel(Overlays, Overlays, DropZone::Center));
    assert_eq!(layout, before);

    // A tab dragged to its own group's edge splits out of it.
    assert!(layout.move_panel(Media, Media, DropZone::Right));
    assert_eq!(group_of(&layout, Media), vec![Media]);
    assert!(
        layout
            .encode_tree()
            .contains("h500([overlays|stingers|playlist],[media])")
    );
    all_shown_once(&layout);
}

#[test]
fn close_and_reopen_restore_panels_to_their_home() {
    let mut layout = DockLayout::default();
    assert!(layout.close(Transitions));
    assert!(!layout.is_visible(Transitions));
    assert!(!layout.close(Transitions));
    assert!(layout.encode_tree().contains("h375([preview],[program])"));
    layout.show(Transitions);
    assert!(
        layout
            .encode_tree()
            .contains("h375(h500([preview],[transitions]),[program])")
    );

    // A side tab returns as a tab of the side group, in front.
    assert!(layout.close(Stingers));
    layout.show(Stingers);
    assert_eq!(
        group_of(&layout, Stingers),
        vec![Overlays, Titles, Playlist, Media, Stingers]
    );
    assert!(layout.is_showing(Stingers));

    // With everything hidden the tree is empty; panels come back along
    // their home edges.
    for panel in PanelId::ALL {
        layout.close(panel);
    }
    assert_eq!(layout.root(), None);
    assert_eq!(layout.encode_tree(), "none");
    layout.show(Mixer);
    layout.show(Preview);
    layout.show(Overlays);
    assert_eq!(
        layout.encode_tree(),
        "h765(v450([preview],[mixer]),[overlays])"
    );
    // Showing a panel behind another tab only brings it to the front.
    layout.show(Titles);
    layout.show(Overlays);
    assert!(layout.is_showing(Overlays));
    assert_eq!(group_of(&layout, Overlays), vec![Overlays, Titles]);
}

#[test]
fn closing_the_shown_tab_shows_its_left_neighbour() {
    let mut layout = DockLayout::default();
    layout.activate(Titles);
    assert!(layout.close(Titles));
    assert!(layout.is_showing(Stingers));
    // Closing a tab behind the shown one keeps the shown one.
    layout.activate(Playlist);
    assert!(layout.close(Overlays));
    assert!(layout.is_showing(Playlist));
    // The first tab hands over to the next one.
    layout.activate(Stingers);
    assert!(layout.close(Stingers));
    assert!(layout.is_showing(Playlist));
    assert_eq!(group_of(&layout, Media), vec![Playlist, Media]);
}

#[test]
fn a_whole_tab_group_moves_with_its_tabs_and_shown_tab() {
    let mut layout = DockLayout::default();
    layout.activate(Titles);
    // Split the side tab group off to the left of the input bank.
    assert!(layout.move_group(Playlist, InputBank, DropZone::Left));
    all_shown_once(&layout);
    assert!(
        layout
            .encode_tree()
            .contains("h500([overlays|stingers|titles*|playlist|media],[inputs])"),
        "{}",
        layout.encode_tree()
    );
    // Its old split collapsed into Recording & streams alone.
    assert!(layout.encode_tree().ends_with(",[outputs])"));
    assert!(layout.is_showing(Titles));

    // Join it into the mixer's group: appended, its shown tab in front.
    assert!(layout.move_group(Overlays, Mixer, DropZone::Center));
    assert_eq!(
        group_of(&layout, Mixer),
        vec![Mixer, Overlays, Stingers, Titles, Playlist, Media]
    );
    assert!(layout.is_showing(Titles));
    all_shown_once(&layout);

    // A group dropped onto itself, or a hidden target, changes nothing.
    let before = layout.clone();
    assert!(!layout.move_group(Mixer, Titles, DropZone::Left));
    assert!(!layout.move_group(Mixer, Mixer, DropZone::Center));
    layout.close(Outputs);
    let before_hidden = layout.clone();
    assert!(!layout.move_group(Mixer, Outputs, DropZone::Top));
    assert_eq!(layout, before_hidden);
    assert_ne!(layout, before);

    // A lone group moves as its panel does.
    let mut lone = DockLayout::default();
    let mut by_panel = DockLayout::default();
    assert!(lone.move_group(Program, Preview, DropZone::Bottom));
    assert!(by_panel.move_panel(Program, Preview, DropZone::Bottom));
    assert_eq!(lone, by_panel);
}

#[test]
fn tab_groups_list_every_docked_group_in_tree_order() {
    let mut layout = DockLayout::default();
    layout.activate(Media);
    let groups = layout.tab_groups();
    assert_eq!(groups.len(), layout.active_tabs().len());
    assert_eq!(groups[0], (vec![Preview], 0));
    assert_eq!(
        groups.last(),
        Some(&(vec![Overlays, Stingers, Titles, Playlist, Media], 4))
    );
    assert_eq!(
        layout.tab_group(Stingers),
        Some(DockNode::Tabs {
            panels: vec![Overlays, Stingers, Titles, Playlist, Media],
            active: 4,
        })
    );
    layout.pop_out(Preview, None);
    assert_eq!(layout.tab_group(Preview), None);
    assert_eq!(layout.tab_groups()[0], (vec![Transitions], 0));
}

#[test]
fn pop_out_and_dock_back_keep_each_panel_in_one_place() {
    let mut layout = DockLayout::default();
    let geometry = WindowGeometry {
        size: (640, 400),
        position: Some((-200, 80)),
    };
    assert!(layout.pop_out(Mixer, Some(geometry)));
    assert!(!layout.pop_out(Mixer, None));
    assert!(layout.is_popped(Mixer) && !layout.is_docked(Mixer));
    assert!(layout.is_showing(Mixer));
    all_shown_once(&layout);
    assert_eq!(layout.encode_popouts(), "mixer:640x400@-200,80");
    layout.set_geometry(
        Mixer,
        WindowGeometry {
            size: (700, 420),
            position: None,
        },
    );
    assert!(layout.pop_out(Program, None));
    assert_eq!(layout.encode_popouts(), "mixer:700x420; program");
    // Showing a popped panel leaves it in its window.
    layout.show(Mixer);
    assert!(layout.is_popped(Mixer));

    assert!(layout.dock_back(Mixer));
    assert!(!layout.dock_back(Mixer));
    assert!(layout.is_docked(Mixer));
    assert_eq!(group_of(&layout, Mixer), vec![Mixer]);
    assert!(layout.encode_tree().contains("h600([inputs],[mixer])"));
    // Closing a popped panel hides it.
    assert!(layout.close(Program));
    assert!(!layout.is_visible(Program));
    assert_eq!(layout.popped(), &[]);
    // A popped panel dragged onto a tile docks there.
    assert!(layout.pop_out(Titles, None));
    assert!(layout.move_panel(Titles, Preview, DropZone::Center));
    assert_eq!(group_of(&layout, Preview), vec![Preview, Titles]);
    assert!(!layout.is_popped(Titles));
    all_shown_once(&layout);
}

#[test]
fn trees_and_popouts_round_trip_through_text() {
    let mut layout = LayoutPreset::Compact.layout();
    layout.activate(Mixer);
    layout.set_ratio(&[true], 333);
    layout.pop_out(
        Stingers,
        Some(WindowGeometry {
            size: (500, 300),
            position: Some((10, -20)),
        }),
    );
    layout.pop_out(Preview, None);
    let tree = layout.encode_tree();
    let popouts = layout.encode_popouts();
    assert_eq!(
        tree,
        "v560(h394([transitions],[program]),h333([inputs|mixer*],[outputs|overlays|titles|playlist|media]))"
    );
    assert_eq!(popouts, "stingers:500x300@10,-20; preview");
    assert_eq!(DockLayout::decode(&tree, &popouts), Ok(layout.clone()));
    for preset in LayoutPreset::ALL {
        let layout = preset.layout();
        assert_eq!(
            DockLayout::decode(&layout.encode_tree(), &layout.encode_popouts()),
            Ok(layout)
        );
    }
    let empty = DockLayout::decode("none", "none").unwrap();
    assert_eq!(empty.root(), None);
    // Ratios are clamped when set.
    layout.set_ratio(&[], 2);
    assert!(layout.encode_tree().starts_with("v50("));

    // Saved layouts carry a name.
    let named = NamedLayout {
        name: "Late show".into(),
        layout: layout.clone(),
    };
    let text = named.encode();
    assert!(text.starts_with("Late show | v50("), "{text}");
    assert_eq!(NamedLayout::decode(&text), Some(named));
}

#[test]
fn malformed_or_duplicated_layouts_are_rejected() {
    for tree in [
        "",
        "[]",
        "[preview",
        "[preview]]",
        "[camera]",
        "[preview*]",
        "[preview|program*|mixer*]",
        "h(  [preview],[program])",
        "h500([preview])",
        "h500([preview],[program]",
        "h40([preview],[program])",
        "h951([preview],[program])",
        "x500([preview],[program])",
        "h+50([preview],[program])",
        "NONE",
    ] {
        assert_eq!(
            DockLayout::decode(tree, "none"),
            Err(DockError::Malformed),
            "{tree}"
        );
    }
    assert_eq!(
        DockLayout::decode("h500([preview],[preview])", "none"),
        Err(DockError::Duplicate)
    );
    assert_eq!(
        DockLayout::decode("[preview|program]", "program"),
        Err(DockError::Duplicate)
    );
    let deep = format!("{}[preview]{}", "h500([mixer],".repeat(20), ")".repeat(20));
    assert_eq!(DockLayout::decode(&deep, "none"), Err(DockError::Malformed));
    for popouts in [
        "",
        "mixer;mixer",
        "mixer:640",
        "mixer:640x",
        "mixer:20x20",
        "mixer:640x400@1",
        "mixer:640x400@1,99999999",
        "camera",
        "mixer,program",
    ] {
        assert_eq!(
            DockLayout::decode("none", popouts),
            Err(DockError::Malformed),
            "{popouts}"
        );
    }
    // A rejected tree or pop-out list leaves the layout as it was.
    let mut layout = DockLayout::default();
    assert_eq!(layout.set_tree("[mixer]["), Err(DockError::Malformed));
    assert_eq!(layout.set_popouts("mixer"), Err(DockError::Duplicate));
    assert_eq!(layout, DockLayout::default());
    assert_eq!(layout.set_tree("[mixer]"), Ok(()));
    assert_eq!(layout.set_popouts("program:300x200"), Ok(()));
    assert_eq!(layout.docked(), vec![Mixer]);

    for text in [
        "",
        "name",
        "a | [mixer]",
        " | [mixer] | none",
        "a|b | [mixer] | none",
    ] {
        assert_eq!(NamedLayout::decode(text), None, "{text}");
    }
}

#[test]
fn layout_names_are_checked() {
    let saved = vec![NamedLayout {
        name: "Show".into(),
        layout: DockLayout::default(),
    }];
    assert_eq!(check_layout_name("Late", &saved, None), Ok(()));
    assert_eq!(
        check_layout_name("", &saved, None),
        Err(LayoutNameProblem::Empty)
    );
    assert_eq!(
        check_layout_name(&"x".repeat(41), &saved, None),
        Err(LayoutNameProblem::TooLong)
    );
    assert_eq!(
        check_layout_name("a|b", &saved, None),
        Err(LayoutNameProblem::BadCharacter)
    );
    assert_eq!(
        check_layout_name("show", &saved, None),
        Err(LayoutNameProblem::Taken)
    );
    // Renaming a layout to its own name (in any case) is fine.
    assert_eq!(check_layout_name("SHOW", &saved, Some(0)), Ok(()));
}

#[test]
fn window_geometry_is_clamped_from_points() {
    assert_eq!(
        WindowGeometry::from_points((50.0, 9_999.0), Some((-40_000.0, 12.4))),
        WindowGeometry {
            size: (120, 8_000),
            position: Some((-32_000, 12)),
        }
    );
    assert_eq!(
        WindowGeometry::from_points((640.4, 360.6), None),
        WindowGeometry {
            size: (640, 361),
            position: None,
        }
    );
}

#[test]
fn unmeasurable_window_points_still_make_a_readable_geometry() {
    let geometry = WindowGeometry::from_points((f32::NAN, 500.0), Some((f32::NAN, 40.0)));
    assert_eq!(WindowGeometry::decode(&geometry.encode()), Some(geometry));
    assert_eq!(geometry.size, (WindowGeometry::MIN_SIDE, 500));
    assert_eq!(geometry.position, None);
    let geometry = WindowGeometry::from_points((800.0, f32::NAN), Some((10.0, f32::INFINITY)));
    assert_eq!(WindowGeometry::decode(&geometry.encode()), Some(geometry));
}
