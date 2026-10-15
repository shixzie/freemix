//! OR-011: display enumeration contracts, display-output configuration, and
//! the hot-plug matching state machine driven by a fake display provider.

use std::num::NonZeroU128;

use fm_io_api::display::{
    ActiveDisplay, DisplayConfigError, DisplayId, DisplayInfo, DisplayMode, DisplayOutputConfig,
    DisplayOutputSet, DisplayOutputSource, DisplayOutputState, DisplayOutputTracker,
    DisplayProvider, DisplaySelector, FakeDisplays, HdrCapability, HdrRequest, HdrState,
    MAX_DISPLAY_OUTPUTS, ModeRequest, WaitReason,
};
use fm_types::{OutputId, OutputScaling};

const HD: DisplayMode = DisplayMode::new(1920, 1080, 60_000);
const UHD: DisplayMode = DisplayMode::new(3840, 2160, 60_000);

fn id(value: &str) -> DisplayId {
    DisplayId::new(value).unwrap()
}

fn display(display_id: &str, name: &str, x: i32) -> DisplayInfo {
    let mut info = DisplayInfo::new(id(display_id), name, HD);
    info.x = x;
    info
}

fn config(spec: &str) -> DisplayOutputConfig {
    spec.parse().unwrap()
}

fn tracker(specs: &[&str]) -> DisplayOutputTracker {
    DisplayOutputTracker::new(
        DisplayOutputSet::new(specs.iter().map(|spec| config(spec)).collect()).unwrap(),
        false,
    )
}

fn active_id(tracker: &DisplayOutputTracker, output: usize) -> Option<String> {
    tracker
        .state(output)
        .and_then(DisplayOutputState::active)
        .map(|active| active.id.to_string())
}

fn waiting(tracker: &DisplayOutputTracker, output: usize) -> Option<WaitReason> {
    match tracker.state(output) {
        Some(DisplayOutputState::Waiting(reason)) => Some(*reason),
        _ => None,
    }
}

#[test]
fn display_output_specs_parse_every_field_and_round_trip() {
    let parsed =
        config("display=name:Left,source=output:7,mode=1280x720@59.94,scaling=1:1,hdr=auto");
    assert_eq!(parsed.display, DisplaySelector::Name("Left".into()));
    assert_eq!(
        parsed.source,
        DisplayOutputSource::Output(OutputId::new(NonZeroU128::new(7).unwrap()))
    );
    assert_eq!(
        parsed.mode,
        ModeRequest::Exact(DisplayMode::new(1280, 720, 59_940))
    );
    assert_eq!(parsed.scaling, OutputScaling::OneToOne);
    assert_eq!(parsed.hdr, HdrRequest::Auto);
    assert_eq!(config(&parsed.to_string()), parsed);

    let defaults = config("display=1");
    assert_eq!(
        defaults,
        DisplayOutputConfig::program_on(DisplaySelector::Index(1))
    );
    assert_eq!(
        config("display=id:cg:42,source=preview").source,
        DisplayOutputSource::Preview
    );
    assert_eq!(
        "1920x1080@60000".parse::<DisplayMode>(),
        Ok(DisplayMode::new(1920, 1080, 60_000))
    );
    assert_eq!(
        "1920x1080".parse::<DisplayMode>(),
        Ok(DisplayMode::new(1920, 1080, 0))
    );

    for (spec, expected) in [
        ("source=program", DisplayConfigError::MissingDisplay),
        (
            "display=0,display=1",
            DisplayConfigError::DuplicateField("display".into()),
        ),
        (
            "display=0,scaling=zoom",
            DisplayConfigError::InvalidScaling("zoom".into()),
        ),
        (
            "display=0,source=output:0",
            DisplayConfigError::InvalidSource("output:0".into()),
        ),
        (
            "display=0,mode=0x1080",
            DisplayConfigError::InvalidMode("0x1080".into()),
        ),
        (
            "display=0,cursor=shown",
            DisplayConfigError::InvalidField("cursor=shown".into()),
        ),
        (
            "display=0,hdr=pq",
            DisplayConfigError::InvalidHdr("pq".into()),
        ),
    ] {
        assert_eq!(spec.parse::<DisplayOutputConfig>(), Err(expected), "{spec}");
    }
}

#[test]
fn output_sets_hold_one_or_two_outputs_on_distinct_selectors() {
    assert_eq!(MAX_DISPLAY_OUTPUTS, 2);
    assert_eq!(
        DisplayOutputSet::new(Vec::new()),
        Err(DisplayConfigError::Empty)
    );
    assert_eq!(
        DisplayOutputSet::new(vec![
            config("display=0"),
            config("display=1"),
            config("display=2")
        ]),
        Err(DisplayConfigError::TooMany {
            maximum: 2,
            actual: 3
        })
    );
    assert_eq!(
        DisplayOutputSet::new(vec![
            config("display=name:A"),
            config("display=name:A,source=preview")
        ]),
        Err(DisplayConfigError::DuplicateSelector(
            DisplaySelector::Name("A".into())
        ))
    );
    let set = DisplayOutputSet::new(vec![config("display=0"), config("display=1")]).unwrap();
    assert_eq!(set.outputs().len(), 2);
}

#[test]
fn two_outputs_bind_independently_with_their_own_source_mode_scaling_and_hdr() {
    let mut left = display("cg:1", "Left", 0);
    left.modes = vec![HD, DisplayMode::new(1280, 720, 60_000)];
    left.hdr = HdrCapability::Hdr;
    let mut right = display("cg:2", "Right", 1920);
    right.current_mode = UHD;
    right.modes = vec![UHD];
    right.hdr = HdrCapability::Sdr;
    let mut provider = FakeDisplays::new(vec![right, left]);

    let mut outputs = tracker(&[
        "display=name:Left,source=program,mode=1280x720@60,scaling=fit,hdr=auto",
        "display=id:cg:2,source=output:2,mode=1920x1080@60,scaling=fill,hdr=auto",
    ]);
    let changes = outputs.reconcile(&provider.displays().unwrap());
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|change| !change.detached()));

    let first = outputs.state(0).unwrap().active().unwrap().clone();
    assert_eq!(
        first,
        ActiveDisplay {
            id: id("cg:1"),
            name: "Left".into(),
            x: 0,
            y: 0,
            mode: DisplayMode::new(1280, 720, 60_000),
            mode_fallback: false,
            hdr: HdrState::SdrPresenterLimited,
        }
    );
    let second = outputs.state(1).unwrap().active().unwrap().clone();
    assert_eq!(second.id, id("cg:2"));
    // 1080p is not offered by the UHD-only display: the current mode is used
    // and the fallback is reported.
    assert_eq!((second.mode, second.mode_fallback), (UHD, true));
    assert_eq!(second.hdr, HdrState::SdrDisplayNotCapable);
    assert_eq!(
        outputs.config(0).unwrap().source,
        DisplayOutputSource::Program
    );
    assert_eq!(outputs.config(1).unwrap().scaling, OutputScaling::Fill);

    // Steady state reports nothing.
    assert_eq!(outputs.reconcile(&provider.displays().unwrap()), Vec::new());
}

#[test]
fn hdr_state_resolves_request_capability_and_presenter() {
    assert_eq!(
        HdrState::resolve(HdrRequest::Off, HdrCapability::Hdr, true),
        HdrState::Sdr
    );
    assert_eq!(
        HdrState::resolve(HdrRequest::Auto, HdrCapability::Unknown, true),
        HdrState::SdrCapabilityUnknown
    );
    assert_eq!(
        HdrState::resolve(HdrRequest::Auto, HdrCapability::Hdr, true),
        HdrState::Hdr
    );
    let mut hdr = display("hdr", "HDR", 0);
    hdr.hdr = HdrCapability::Hdr;
    let mut outputs = DisplayOutputTracker::new(
        DisplayOutputSet::new(vec![config("display=0,hdr=auto")]).unwrap(),
        true,
    );
    outputs.reconcile(&[hdr]);
    assert_eq!(
        outputs.state(0).unwrap().active().unwrap().hdr,
        HdrState::Hdr
    );
}

#[test]
fn unplugged_display_waits_and_the_output_reattaches_by_stable_id() {
    let provider = FakeDisplays::new(vec![
        display("x:10", "HDMI-1", 0),
        display("x:11", "DP-1", 1920),
    ]);
    let mut enumerate = provider.clone();
    let mut outputs = tracker(&["display=0", "display=1,source=output:2"]);
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("x:10"));
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("x:11"));

    let removed = provider.disconnect(&id("x:10")).unwrap();
    let changes = outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].output, 0);
    assert!(changes[0].detached());
    assert_eq!(waiting(&outputs, 0), Some(WaitReason::DisplayAbsent));
    // The other output is independent and keeps its display; the waiting
    // output never jumps to the display now at index 0.
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("x:11"));

    provider.connect(removed);
    let changes = outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(changes.len(), 1);
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("x:10"));
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("x:11"));
}

#[test]
fn a_returning_display_with_a_new_platform_id_is_matched_by_name() {
    let provider = FakeDisplays::new(vec![display("cg:5", "Studio Display", 0)]);
    let mut enumerate = provider.clone();
    let mut outputs = tracker(&["display=id:cg:5"]);
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("cg:5"));

    provider.replace(Vec::new());
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(waiting(&outputs, 0), Some(WaitReason::DisplayAbsent));

    // The platform re-numbered the display; its name still identifies it.
    provider.replace(vec![display("cg:9", "Studio Display", 0)]);
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("cg:9"));
    // Once rebound, the new id is authoritative even if a same-named display appears.
    provider.replace(vec![
        display("cg:9", "Studio Display", 0),
        display("cg:10", "Studio Display", 1920),
    ]);
    assert_eq!(
        outputs.reconcile(&enumerate.displays().unwrap()),
        Vec::new()
    );
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("cg:9"));
}

#[test]
fn duplicate_names_without_the_bound_id_are_ambiguous_not_guessed() {
    let mut outputs = tracker(&["display=name:LG"]);
    outputs.reconcile(&[display("a", "LG", 0), display("b", "LG", 1920)]);
    assert_eq!(waiting(&outputs, 0), Some(WaitReason::AmbiguousName));
    outputs.reconcile(&[display("b", "LG", 1920)]);
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("b"));
}

#[test]
fn two_outputs_never_share_a_display() {
    let mut outputs = tracker(&["display=id:a", "display=name:Shared"]);
    outputs.reconcile(&[display("a", "Shared", 0)]);
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("a"));
    assert_eq!(waiting(&outputs, 1), Some(WaitReason::DisplayInUse));
}

#[test]
fn mode_changes_and_enumeration_failures_are_reported_and_recovered() {
    let provider = FakeDisplays::new(vec![display("a", "A", 0)]);
    let mut enumerate = provider.clone();
    let mut outputs = tracker(&["display=0"]);
    outputs.reconcile(&enumerate.displays().unwrap());

    let mut resized = display("a", "A", 0);
    resized.current_mode = UHD;
    provider.connect(resized);
    let changes = outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(changes.len(), 1);
    assert!(!changes[0].detached());
    assert_eq!(outputs.state(0).unwrap().active().unwrap().mode, UHD);

    provider.fail_with(Some("display server restarting".into()));
    let error = enumerate.displays().unwrap_err();
    assert!(error.to_string().contains("display server restarting"));
    let changes = outputs.enumeration_failed();
    assert!(changes[0].detached());
    assert_eq!(waiting(&outputs, 0), Some(WaitReason::EnumerationFailed));

    provider.fail_with(None);
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("a"));
}

#[test]
fn index_selection_uses_deterministic_desktop_order_and_waits_when_absent() {
    let mut outputs = tracker(&["display=1"]);
    outputs.reconcile(&[display("b", "B", 1920), display("a", "A", 0)]);
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("b"));

    let mut absent = tracker(&["display=3"]);
    absent.reconcile(&[display("a", "A", 0)]);
    assert_eq!(waiting(&absent, 0), Some(WaitReason::DisplayAbsent));
    assert!(!absent.is_bound(0));
}

#[test]
fn source_can_change_without_rebinding() {
    let mut outputs = tracker(&["display=0"]);
    outputs.reconcile(&[display("a", "A", 0)]);
    assert!(outputs.set_source(0, DisplayOutputSource::Preview));
    assert_eq!(
        outputs.config(0).unwrap().source,
        DisplayOutputSource::Preview
    );
    assert_eq!(outputs.reconcile(&[display("a", "A", 0)]), Vec::new());
    assert!(!outputs.set_source(5, DisplayOutputSource::Program));
}

#[test]
fn unplugging_one_of_two_identical_displays_never_moves_its_output_onto_the_other() {
    // Two identical monitors share a model name; each output is bound to
    // its own by id.
    let provider = FakeDisplays::new(vec![
        display("a", "DELL U2720Q", 0),
        display("b", "DELL U2720Q", 1920),
    ]);
    let mut enumerate = provider.clone();
    let mut outputs = tracker(&["display=0", "display=1"]);
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("a"));
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("b"));

    // Unplugging `a` must not let output 0 follow the name onto `b`, which
    // output 1 is showing.
    let removed = provider.disconnect(&id("a")).unwrap();
    let changes = outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].output, 0);
    assert_eq!(waiting(&outputs, 0), Some(WaitReason::DisplayAbsent));
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("b"));

    // Both return to their own displays when `a` comes back.
    provider.connect(removed);
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("a"));
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("b"));

    // A re-numbered `a` is still found by name: `b` belongs to output 1.
    let mut renumbered = provider.disconnect(&id("a")).unwrap();
    outputs.reconcile(&enumerate.displays().unwrap());
    renumbered.id = id("a2");
    provider.connect(renumbered);
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("a2"));
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("b"));
}

#[test]
fn a_hot_plugged_display_never_takes_a_bound_output_away_by_index() {
    // Output 0 wants the second display, output 1 the first; only one is
    // connected, so output 1 binds it and output 0 waits.
    let provider = FakeDisplays::new(vec![display("a", "A", 0)]);
    let mut enumerate = provider.clone();
    let mut outputs = tracker(&["display=1", "display=0"]);
    outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(waiting(&outputs, 0), Some(WaitReason::DisplayAbsent));
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("a"));

    // A display plugged in to the left of `a` makes `a` index 1. Output 1
    // keeps showing `a`; output 0 must not claim it from under it.
    provider.connect(display("left", "Left", -1920));
    let changes = outputs.reconcile(&enumerate.displays().unwrap());
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("a"));
    assert!(changes.iter().all(|change| change.output != 1));
    assert_eq!(waiting(&outputs, 0), Some(WaitReason::DisplayInUse));
    assert!(!outputs.is_bound(0));
}

#[test]
fn slots_configure_live_keeping_the_binding_unless_the_display_changes() {
    use fm_io_api::display::DisplayOutputReconfiguration as Change;
    let displays = [display("a", "A", 0), display("b", "B", 1920)];
    let mut outputs = DisplayOutputTracker::empty(false);
    assert!(outputs.is_empty());
    assert_eq!(outputs.configure(1, None), Change::Unchanged);
    assert_eq!(
        outputs.configure(1, Some(config("display=id:b,source=preview"))),
        Change::Rebinding
    );
    assert_eq!(waiting(&outputs, 1), Some(WaitReason::DisplayAbsent));
    outputs.reconcile(&displays);
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("b"));
    assert_eq!(outputs.config(0), None);

    // Same display, other fields: retargeted in place, still bound.
    assert_eq!(
        outputs.configure(1, Some(config("display=id:b,source=program,scaling=fill"))),
        Change::Retargeted
    );
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("b"));
    assert_eq!(outputs.reconcile(&displays), Vec::new());
    assert_eq!(
        outputs.configure(1, Some(config("display=id:b,source=program,scaling=fill"))),
        Change::Unchanged
    );
    // Another display rebinds on the next reconcile.
    assert_eq!(
        outputs.configure(1, Some(config("display=id:a"))),
        Change::Rebinding
    );
    outputs.reconcile(&displays);
    assert_eq!(active_id(&outputs, 1).as_deref(), Some("a"));
    assert_eq!(outputs.configure(1, None), Change::Cleared);
    assert!(outputs.is_empty());
    assert_eq!(
        outputs.configure(MAX_DISPLAY_OUTPUTS, Some(config("display=id:a"))),
        Change::Unchanged
    );
}

#[test]
fn an_id_with_a_name_binds_by_id_then_falls_back_to_the_unique_name() {
    let remembered = DisplayOutputConfig::program_on(DisplaySelector::IdOrName {
        id: id("old-id"),
        name: "Wall".into(),
    });
    let mut outputs = DisplayOutputTracker::empty(false);
    outputs.configure(0, Some(remembered.clone()));
    // The id is gone (a renumbered platform id): the unique name binds.
    outputs.reconcile(&[display("a", "Desk", 0), display("new-id", "Wall", 1920)]);
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("new-id"));

    let mut outputs = DisplayOutputTracker::empty(false);
    outputs.configure(0, Some(remembered));
    // The id wins over another display that has the name.
    outputs.reconcile(&[display("x", "Wall", 0), display("old-id", "Renamed", 1920)]);
    assert_eq!(active_id(&outputs, 0).as_deref(), Some("old-id"));
}
