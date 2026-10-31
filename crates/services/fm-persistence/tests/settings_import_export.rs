use std::{collections::BTreeMap, num::NonZeroU128};

use fm_model::{
    AudioBus, Input, InputKind, Output, Project, ProjectSettings, RestartPolicy, Rgba8, Scene,
    StartupPolicy, StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId,
};
use fm_persistence::{
    ChangeKind, ChangePreview, ContentInventory, EntityKey, ImportConflict, ImportError,
    ImportPlan, ImportResolutions, MAX_SETTINGS_EXPORT_BYTES, MergeStrategy, ReferenceKind,
    SETTINGS_EXPORT_CONTRACT, Secret, SecretSlot, SectionChoice, SettingKey, SettingKeyError,
    SettingReference, SettingValue, SettingsExportError, SettingsSection, SettingsSnapshot,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};

const STREAM_KEY: &str = "live-7c1e-super-secret";

fn nz(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn key(path: &str) -> SettingKey {
    SettingKey::parse(path).unwrap()
}

fn entity(section: SettingsSection, id: Option<u128>) -> EntityKey {
    EntityKey::new(section, id.map(nz)).unwrap()
}

fn settings() -> ProjectSettings {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    ProjectSettings {
        frame_rate,
        video: VideoFormat {
            dimensions: VideoDimensions::new(1_920, 1_080).unwrap(),
            frame_rate,
            pixel_format: PixelFormat::Nv12,
            scan: ScanMode::Progressive,
            color: ColorMetadata::default(),
        },
        audio: AudioFormat {
            sample_rate: SampleRate::new(48_000).unwrap(),
            sample_format: SampleFormat::F32,
            channels: ChannelLayout::stereo(),
        },
    }
}

fn project() -> Project {
    let mut project = Project::new(ProjectId::new(nz(1)), "Show", settings())
        .with_restart_policy(RestartPolicy::OnFailure { max_attempts: 3 });
    project.add_input(Input {
        id: InputId::new(nz(1)),
        name: "Color".into(),
        kind: InputKind::Color,
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: SceneId::new(nz(10)),
        name: "Main".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_audio_bus(AudioBus {
        id: BusId::new(nz(20)),
        name: "Master".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: OutputId::new(nz(30)),
        name: "Program".into(),
        video_source: SceneId::new(nz(10)),
        audio_source: BusId::new(nz(20)),
        startup: StartupPolicy::Stopped,
        required_capabilities: Vec::new(),
    });
    project.add_stream_target(
        StreamTarget::new(
            StreamTargetId::new(nz(40)),
            "Main ingest".into(),
            StreamProtocol::Rtmps,
            StreamEndpoint::parse("ingest.example.test/live").unwrap(),
            Some(StreamKey::parse(STREAM_KEY).unwrap()),
            OutputId::new(nz(30)),
        )
        .unwrap(),
    );
    project
}

fn current() -> SettingsSnapshot {
    SettingsSnapshot::from_project(&project()).unwrap()
}

fn inventory() -> ContentInventory {
    ContentInventory::from_project(&project())
}

fn exported() -> SettingsSnapshot {
    SettingsSnapshot::parse_export_json(&current().to_export_json()).unwrap()
}

fn with(mut snapshot: SettingsSnapshot, path: &str, value: SettingValue) -> SettingsSnapshot {
    snapshot.insert(key(path), value).unwrap();
    snapshot
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.into())
}

fn without_entity(snapshot: &SettingsSnapshot, removed: EntityKey) -> SettingsSnapshot {
    SettingsSnapshot::from_entries(
        snapshot
            .entries()
            .filter(|(key, _)| key.entity() != removed)
            .map(|(key, value)| (key.clone(), value.clone())),
    )
    .unwrap()
}

#[test]
fn export_document_is_exact_deterministic_and_secret_free() {
    let snapshot = current();
    assert!(matches!(
        snapshot.get_path("stream_targets.40.key"),
        Some(SettingValue::Secret(SecretSlot::Present(_)))
    ));
    let json = snapshot.to_export_json();
    assert!(!json.contains(STREAM_KEY));
    assert!(!format!("{snapshot:?}").contains(STREAM_KEY));
    assert_eq!(
        json,
        SettingsSnapshot::from_project(&project())
            .unwrap()
            .to_export_json()
    );
    let expected = format!(
        "{{\n  \"contract\": \"{SETTINGS_EXPORT_CONTRACT}\",\n  \"entries\": {{\n\
\x20   \"audio_buses.20.name\": {{\"text\": \"Master\"}},\n\
\x20   \"format.audio.channel_count\": {{\"integer\": 2}},\n\
\x20   \"format.audio.sample_rate_hz\": {{\"integer\": 48000}},\n\
\x20   \"format.frame_rate.denominator\": {{\"integer\": 1}},\n\
\x20   \"format.frame_rate.numerator\": {{\"integer\": 30}},\n\
\x20   \"format.video.height\": {{\"integer\": 1080}},\n\
\x20   \"format.video.width\": {{\"integer\": 1920}},\n\
\x20   \"outputs.30.audio_source\": {{\"reference\": {{\"kind\": \"audio_bus\", \"id\": 20}}}},\n\
\x20   \"outputs.30.name\": {{\"text\": \"Program\"}},\n\
\x20   \"outputs.30.startup\": {{\"text\": \"stopped\"}},\n\
\x20   \"outputs.30.video_source\": {{\"reference\": {{\"kind\": \"scene\", \"id\": 10}}}},\n\
\x20   \"restart_policy.max_attempts\": {{\"integer\": 3}},\n\
\x20   \"restart_policy.mode\": {{\"text\": \"on_failure\"}},\n\
\x20   \"stream_targets.40.endpoint\": {{\"text\": \"ingest.example.test/live\"}},\n\
\x20   \"stream_targets.40.key\": {{\"secret_ref\": \"stream_targets.40.key\"}},\n\
\x20   \"stream_targets.40.name\": {{\"text\": \"Main ingest\"}},\n\
\x20   \"stream_targets.40.output\": {{\"reference\": {{\"kind\": \"output\", \"id\": 30}}}},\n\
\x20   \"stream_targets.40.protocol\": {{\"text\": \"rtmps\"}},\n\
\x20   \"stream_targets.40.startup\": {{\"text\": \"stopped\"}},\n\
\x20   \"stream_targets.40.video_bitrate_kbps\": {{\"integer\": 4500}}\n  }}\n}}\n"
    );
    assert_eq!(json, expected);

    let parsed = SettingsSnapshot::parse_export_json(&json).unwrap();
    assert_eq!(
        parsed.get_path("stream_targets.40.key"),
        Some(&SettingValue::Secret(SecretSlot::Placeholder))
    );
    assert_eq!(parsed.to_export_json(), json);
    assert_eq!(parsed.len(), snapshot.len());
}

#[test]
fn credentials_cannot_be_smuggled_as_plain_values() {
    let mut snapshot = SettingsSnapshot::new();
    let error = snapshot
        .insert(key("stream_targets.40.key"), text(STREAM_KEY))
        .unwrap_err();
    assert!(matches!(error, SettingsExportError::InvalidValue { .. }));
    let secret = Secret::new(STREAM_KEY).unwrap();
    assert_eq!(format!("{secret:?}"), "Secret(****)");
    let error = snapshot
        .insert(
            key("stream_targets.40.name"),
            SettingValue::Secret(SecretSlot::Present(secret)),
        )
        .unwrap_err();
    assert!(matches!(error, SettingsExportError::InvalidValue { .. }));

    let smuggled = format!(
        "{{\"contract\": \"{SETTINGS_EXPORT_CONTRACT}\", \"entries\": {{\"stream_targets.40.key\": {{\"text\": \"{STREAM_KEY}\"}}}}}}"
    );
    assert!(matches!(
        SettingsSnapshot::parse_export_json(&smuggled),
        Err(SettingsExportError::InvalidValue { .. })
    ));
    let wrong_ref = format!(
        "{{\"contract\": \"{SETTINGS_EXPORT_CONTRACT}\", \"entries\": {{\"stream_targets.40.key\": {{\"secret_ref\": \"stream_targets.41.key\"}}}}}}"
    );
    assert!(matches!(
        SettingsSnapshot::parse_export_json(&wrong_ref),
        Err(SettingsExportError::InvalidValue { .. })
    ));
}

#[test]
fn parser_refuses_malformed_documents_with_explicit_errors() {
    let doc = |entries: &str| {
        format!("{{\"contract\": \"{SETTINGS_EXPORT_CONTRACT}\", \"entries\": {{{entries}}}}}")
    };
    assert!(matches!(
        SettingsSnapshot::parse_export_json("{\"contract\": \"other\", \"entries\": {}}"),
        Err(SettingsExportError::ContractMismatch)
    ));
    assert!(matches!(
        SettingsSnapshot::parse_export_json(&format!(
            "{{\"contract\": \"{SETTINGS_EXPORT_CONTRACT}\", \"entries\": {{}}, \"extra\": 1}}"
        )),
        Err(SettingsExportError::Syntax { .. })
    ));
    assert!(matches!(
        SettingsSnapshot::parse_export_json("{"),
        Err(SettingsExportError::Syntax { .. })
    ));
    assert_eq!(
        SettingsSnapshot::parse_export_json(&doc("\"lighting.1.name\": {\"text\": \"x\"}")),
        Err(SettingsExportError::InvalidKey {
            key: "lighting.1.name".into(),
            error: SettingKeyError::UnknownSection,
        })
    );
    assert!(matches!(
        SettingsSnapshot::parse_export_json(&doc("\"outputs.030.name\": {\"text\": \"x\"}")),
        Err(SettingsExportError::InvalidKey {
            error: SettingKeyError::InvalidEntityId,
            ..
        })
    ));
    assert!(matches!(
        SettingsSnapshot::parse_export_json(&doc("\"outputs.30.name\": {\"colour\": \"x\"}")),
        Err(SettingsExportError::InvalidValue { .. })
    ));
    assert!(matches!(
        SettingsSnapshot::parse_export_json(&doc(
            "\"outputs.30.name\": {\"text\": \"x\"}, \"outputs.30.name\": {\"text\": \"y\"}"
        )),
        Err(SettingsExportError::Syntax { .. })
    ));
    let huge = " ".repeat(MAX_SETTINGS_EXPORT_BYTES + 1);
    assert!(matches!(
        SettingsSnapshot::parse_export_json(&huge),
        Err(SettingsExportError::TooLarge { .. })
    ));
    assert_eq!(
        SettingKey::parse("Outputs.1.name"),
        Err(SettingKeyError::InvalidCharacter)
    );
    assert_eq!(
        SettingKey::parse("outputs.1"),
        Err(SettingKeyError::TooFewSegments)
    );
    assert_eq!(
        SettingKey::parse("format.a.b.c.d"),
        Err(SettingKeyError::TooManySegments)
    );
}

#[test]
fn preview_classifies_every_key_path() {
    let incoming = with(exported(), "outputs.30.name", text("Clean feed"));
    let incoming = with(incoming, "audio_buses.21.name", text("Aux"));
    let incoming = without_entity(&incoming, entity(SettingsSection::RestartPolicy, None));
    let plan = ImportPlan::new(
        &current(),
        incoming,
        inventory(),
        MergeStrategy::MergePreferIncoming,
    );
    let preview = plan.incoming_preview();
    assert_eq!(
        preview.kind_of("outputs.30.name"),
        Some(ChangeKind::Changed)
    );
    assert_eq!(
        preview.kind_of("audio_buses.21.name"),
        Some(ChangeKind::Added)
    );
    assert_eq!(
        preview.kind_of("restart_policy.mode"),
        Some(ChangeKind::Removed)
    );
    // A placeholder never reveals whether the secret differs.
    assert_eq!(
        preview.kind_of("stream_targets.40.key"),
        Some(ChangeKind::Unchanged)
    );
    assert_eq!(preview.count(ChangeKind::Added), 1);
    assert_eq!(preview.count(ChangeKind::Changed), 1);
    assert_eq!(preview.count(ChangeKind::Removed), 2);
    assert_eq!(preview.changes().len(), current().len() + 1);
}

#[test]
fn round_trip_import_retains_current_secret_and_changes_nothing() {
    let mut snapshot = current();
    let plan = ImportPlan::from_export_json(
        &snapshot,
        &snapshot.to_export_json(),
        inventory(),
        MergeStrategy::ReplaceAll,
    )
    .unwrap();
    let outcome = plan.outcome().unwrap();
    assert_eq!(outcome.conflicts, Vec::new());
    assert_eq!(outcome.retained_secrets, vec![key("stream_targets.40.key")]);
    let preview = plan
        .apply(&mut snapshot, &ImportResolutions::new())
        .unwrap();
    assert!(!preview.has_changes());
    assert_eq!(snapshot, current());
    let Some(SettingValue::Secret(SecretSlot::Present(secret))) =
        snapshot.get_path("stream_targets.40.key")
    else {
        panic!("secret must be retained");
    };
    assert_eq!(secret.expose_secret(), STREAM_KEY);
}

#[test]
fn merge_strategies_choose_whole_entities() {
    let incoming = with(
        exported(),
        "outputs.30.startup",
        text("reconcile_desired_state"),
    );
    let incoming = with(incoming, "audio_buses.21.name", text("Aux"));
    let incoming = without_entity(&incoming, entity(SettingsSection::Format, None));
    let base = current();
    let result = |strategy| {
        ImportPlan::new(&base, incoming.clone(), inventory(), strategy)
            .outcome()
            .unwrap()
            .result
    };

    let replaced = result(MergeStrategy::ReplaceAll);
    assert_eq!(replaced.get_path("format.video.width"), None);
    assert_eq!(
        replaced.get_path("outputs.30.startup"),
        Some(&text("reconcile_desired_state"))
    );

    let prefer_incoming = result(MergeStrategy::MergePreferIncoming);
    assert!(prefer_incoming.get_path("format.video.width").is_some());
    assert_eq!(
        prefer_incoming.get_path("outputs.30.startup"),
        Some(&text("reconcile_desired_state"))
    );
    assert_eq!(
        prefer_incoming.get_path("audio_buses.21.name"),
        Some(&text("Aux"))
    );

    let prefer_current = result(MergeStrategy::MergePreferCurrent);
    assert_eq!(
        prefer_current.get_path("outputs.30.startup"),
        Some(&text("stopped"))
    );
    assert_eq!(
        prefer_current.get_path("audio_buses.21.name"),
        Some(&text("Aux"))
    );

    let per_section = result(MergeStrategy::PerSection(BTreeMap::from([(
        SettingsSection::AudioBuses,
        SectionChoice::PreferIncoming,
    )])));
    assert_eq!(
        per_section.get_path("outputs.30.startup"),
        Some(&text("stopped"))
    );
    assert_eq!(
        per_section.get_path("audio_buses.21.name"),
        Some(&text("Aux"))
    );
    assert!(per_section.get_path("format.video.width").is_some());
}

#[test]
fn new_secret_needs_reentry_and_apply_is_atomic() {
    let mut incoming = exported();
    for (path, value) in [
        ("stream_targets.41.name", text("Backup ingest")),
        ("stream_targets.41.protocol", text("rtmp")),
        (
            "stream_targets.41.endpoint",
            text("backup.example.test/live"),
        ),
        (
            "stream_targets.41.key",
            SettingValue::Secret(SecretSlot::Placeholder),
        ),
        (
            "stream_targets.41.output",
            SettingValue::Reference(SettingReference {
                kind: ReferenceKind::Output,
                id: nz(30),
            }),
        ),
    ] {
        incoming = with(incoming, path, value);
    }
    let mut snapshot = current();
    let plan = ImportPlan::new(
        &snapshot,
        incoming,
        inventory(),
        MergeStrategy::MergePreferIncoming,
    );
    assert_eq!(
        plan.outcome().unwrap().conflicts,
        vec![ImportConflict::SecretReentryRequired {
            key: key("stream_targets.41.key")
        }]
    );

    let error = plan
        .apply(&mut snapshot, &ImportResolutions::new())
        .unwrap_err();
    assert!(
        matches!(error, ImportError::UnresolvedConflicts(ref conflicts) if conflicts.len() == 1)
    );
    assert_eq!(
        snapshot,
        current(),
        "a refused apply must not change anything"
    );

    let wrong = ImportResolutions::new()
        .with_secret(key("stream_targets.99.key"), Secret::new("abcd").unwrap());
    assert_eq!(
        plan.apply(&mut snapshot, &wrong),
        Err(ImportError::UnexpectedSecret {
            key: key("stream_targets.99.key")
        })
    );

    let resolutions = ImportResolutions::new().with_secret(
        key("stream_targets.41.key"),
        Secret::new("backup-key-1234").unwrap(),
    );
    let preview = plan.apply(&mut snapshot, &resolutions).unwrap();
    assert_eq!(preview.count(ChangeKind::Added), 5);
    assert_eq!(preview.count(ChangeKind::Changed), 0);
    let Some(SettingValue::Secret(SecretSlot::Present(secret))) =
        snapshot.get_path("stream_targets.41.key")
    else {
        panic!("re-entered secret must be applied");
    };
    assert_eq!(secret.expose_secret(), "backup-key-1234");

    // The plan was computed against the old base; it cannot be re-applied.
    assert_eq!(
        plan.apply(&mut snapshot, &resolutions),
        Err(ImportError::StaleBase)
    );
}

#[test]
fn unknown_references_and_id_collisions_are_reported_and_resolvable() {
    let incoming = with(exported(), "outputs.31.name", text("Recorder feed"));
    let incoming = with(
        incoming,
        "outputs.31.video_source",
        SettingValue::Reference(SettingReference {
            kind: ReferenceKind::Scene,
            id: nz(99),
        }),
    );
    let incoming = with(
        incoming,
        "outputs.31.audio_source",
        SettingValue::Reference(SettingReference {
            kind: ReferenceKind::AudioBus,
            id: nz(77),
        }),
    );
    let incoming = with(incoming, "audio_buses.20.name", text("Stage"));
    let mut snapshot = current();
    let plan = ImportPlan::new(
        &snapshot,
        incoming,
        inventory(),
        MergeStrategy::MergePreferIncoming,
    );
    let conflicts = plan.outcome().unwrap().conflicts;
    assert_eq!(
        conflicts,
        vec![
            ImportConflict::IdCollision {
                entity: entity(SettingsSection::AudioBuses, Some(20)),
                current_name: "Master".into(),
                incoming_name: "Stage".into(),
            },
            ImportConflict::UnknownReference {
                key: key("outputs.31.audio_source"),
                reference: SettingReference {
                    kind: ReferenceKind::AudioBus,
                    id: nz(77),
                },
            },
            ImportConflict::UnknownReference {
                key: key("outputs.31.video_source"),
                reference: SettingReference {
                    kind: ReferenceKind::Scene,
                    id: nz(99),
                },
            },
        ]
    );
    assert!(
        conflicts
            .iter()
            .all(|conflict| !conflict.to_string().is_empty())
    );

    assert_eq!(
        plan.apply(
            &mut snapshot,
            &ImportResolutions::new().accept_collision(entity(SettingsSection::Outputs, Some(30)))
        ),
        Err(ImportError::UnknownCollision {
            entity: entity(SettingsSection::Outputs, Some(30))
        })
    );
    assert_eq!(
        plan.apply(
            &mut snapshot,
            &ImportResolutions::new().skip_entity(entity(SettingsSection::Outputs, Some(77)))
        ),
        Err(ImportError::UnknownSkip {
            entity: entity(SettingsSection::Outputs, Some(77))
        })
    );

    let resolutions = ImportResolutions::new()
        .accept_collision(entity(SettingsSection::AudioBuses, Some(20)))
        .skip_entity(entity(SettingsSection::Outputs, Some(31)));
    let preview = plan.apply(&mut snapshot, &resolutions).unwrap();
    assert_eq!(
        preview.kind_of("audio_buses.20.name"),
        Some(ChangeKind::Changed)
    );
    assert_eq!(preview.kind_of("outputs.31.name"), None);
    assert_eq!(
        snapshot.get_path("audio_buses.20.name"),
        Some(&text("Stage"))
    );
}

#[test]
fn replacing_a_section_that_strands_references_is_a_conflict() {
    let incoming = without_entity(&exported(), entity(SettingsSection::AudioBuses, Some(20)));
    let incoming = with(incoming, "audio_buses.21.name", text("Aux"));
    let plan = ImportPlan::new(
        &current(),
        incoming,
        inventory(),
        MergeStrategy::PerSection(BTreeMap::from([(
            SettingsSection::AudioBuses,
            SectionChoice::TakeIncoming,
        )])),
    );
    let outcome = plan.outcome().unwrap();
    assert_eq!(
        outcome.conflicts,
        vec![ImportConflict::UnknownReference {
            key: key("outputs.30.audio_source"),
            reference: SettingReference {
                kind: ReferenceKind::AudioBus,
                id: nz(20),
            },
        }]
    );
    assert_eq!(
        outcome.preview,
        ChangePreview::between(&current(), &outcome.result)
    );
    assert_eq!(
        outcome.preview.kind_of("audio_buses.20.name"),
        Some(ChangeKind::Removed)
    );
}

#[test]
fn apply_updates_an_existing_stream_target_and_preserves_its_key() {
    let snapshot = with(current(), "stream_targets.40.name", text("Backup ingest"));
    let mut project = project();
    snapshot.apply_exported(&mut project).unwrap();
    let target = project
        .stream_targets()
        .iter()
        .find(|target| target.id().get() == nz(40))
        .unwrap();
    assert_eq!(target.name(), "Backup ingest");
    assert!(target.key().is_some());
}

#[test]
fn apply_refuses_stream_target_additions_and_protocol_changes() {
    let mut project = project();
    let added = with(current(), "stream_targets.41.name", text("New"));
    assert!(added.apply_exported(&mut project).is_err());
    let changed = with(current(), "stream_targets.40.protocol", text("rtmp"));
    assert!(changed.apply_exported(&mut project).is_err());
}
