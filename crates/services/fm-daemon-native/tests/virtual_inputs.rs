//! IN-024 / IN-025: the native project plan follows alias chains, routes
//! mix feeds to the synthetic black scene, and refuses crops and cycles.

use std::num::NonZeroU128;

use fm_daemon_native::native_media::{
    NativeProjectLimits, NativeProjectPlan, NativeProjectPlanError, NativeVideoRoute,
};
use fm_model::{
    Input, InputKind, Project, ProjectSettings, Rgba8, Scene, SceneAnimation, SimulatedAudio,
    SimulatedInput, SimulatedVideo,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn scene(value: u128) -> SceneId {
    SceneId::new(id(value))
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    Project::new(
        ProjectId::new(id(1)),
        "native aliases",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(64, 36).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(48_000).unwrap(),
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        },
    )
}

fn add_leaf(project: &mut Project, value: u128) {
    project.add_input(Input {
        id: input(value),
        name: format!("leaf {value}"),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
}

fn add_alias(project: &mut Project, value: u128, target: u128, crop: Option<fm_model::CropRect>) {
    project.add_input(Input {
        id: input(value),
        name: format!("alias {value}"),
        kind: InputKind::Alias {
            target: input(target),
            crop,
        },
        required_capabilities: Vec::new(),
    });
}

fn add_feed(project: &mut Project, value: u128, feed: fm_model::MixFeed) {
    project.add_input(Input {
        id: input(value),
        name: format!("feed {value}"),
        kind: InputKind::MixFeed(feed),
        required_capabilities: Vec::new(),
    });
}

#[test]
fn aliases_follow_their_chain_and_feeds_use_a_black_scene() {
    let mut project = project();
    add_leaf(&mut project, 1);
    add_leaf(&mut project, 2);
    add_alias(&mut project, 3, 1, None);
    add_alias(&mut project, 4, 3, None);
    add_feed(&mut project, 5, fm_model::MixFeed::Program);

    let plan = NativeProjectPlan::compile(&project, NativeProjectLimits::default()).unwrap();
    assert_eq!(
        plan.video_route(input(3)),
        Some(NativeVideoRoute::Leaf(input(1)))
    );
    assert_eq!(
        plan.video_route(input(4)),
        Some(NativeVideoRoute::Leaf(input(1)))
    );
    let Some(NativeVideoRoute::Scene(black)) = plan.video_route(input(5)) else {
        panic!("a feed routes to the synthetic black scene");
    };
    assert!(plan.scene_order().any(|scene| scene == black));

    // An alias to a scene keeps the scene route under the alias's id.
    let scene_input = input(6);
    project.add_input(Input {
        id: scene_input,
        name: "scene".into(),
        kind: InputKind::Scene {
            scene_id: scene(10),
            audio_source: None,
        },
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene(10),
        name: "scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: SceneAnimation::default(),
    });
    add_alias(&mut project, 7, 6, None);
    let baked_scene = scene(10);
    let plan = NativeProjectPlan::compile(&project, NativeProjectLimits::default()).unwrap();
    assert_eq!(
        plan.video_route(input(7)),
        Some(NativeVideoRoute::Scene(baked_scene))
    );
    assert!(plan.scene_order().any(|scene| scene == baked_scene));
}

#[test]
fn a_cropped_alias_is_refused_instead_of_drawing_the_uncropped_source() {
    let mut project = project();
    add_leaf(&mut project, 1);
    add_alias(
        &mut project,
        3,
        1,
        Some(fm_model::CropRect::new(0, 0, 8, 8)),
    );
    assert_eq!(
        NativeProjectPlan::compile(&project, NativeProjectLimits::default()),
        Err(NativeProjectPlanError::AliasCropUnsupported { input: input(3) })
    );
}

#[test]
fn an_alias_cycle_is_refused() {
    let mut project = project();
    add_alias(&mut project, 1, 2, None);
    add_alias(&mut project, 2, 1, None);
    assert!(matches!(
        NativeProjectPlan::compile(&project, NativeProjectLimits::default()),
        Err(NativeProjectPlanError::AliasCycle { .. })
    ));
}

#[test]
fn an_alias_to_a_missing_input_is_refused() {
    let mut project = project();
    add_alias(&mut project, 1, 99, None);
    assert_eq!(
        NativeProjectPlan::compile(&project, NativeProjectLimits::default()),
        Err(NativeProjectPlanError::MissingInput(input(99)))
    );
}

#[test]
fn output_ids_stay_stable_with_feed_inputs() {
    let mut project = project();
    add_leaf(&mut project, 1);
    add_feed(&mut project, 2, fm_model::MixFeed::Preview);
    let output = OutputId::new(id(10));
    project.add_output(fm_model::Output {
        id: output,
        name: "out".into(),
        video_source: scene(10),
        audio_source: fm_types::BusId::new(id(20)),
        startup: fm_model::StartupPolicy::Stopped,
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene(10),
        name: "scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: SceneAnimation::default(),
    });
    let plan = NativeProjectPlan::compile(&project, NativeProjectLimits::default()).unwrap();
    assert_eq!(plan.output_ids(), &[output]);
}
