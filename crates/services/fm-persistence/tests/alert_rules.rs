//! RC-015: a show's alert rule settings round-trip exactly and strictly in
//! `project.json`; an empty list is omitted, a rule left at its default is
//! not kept, and settings out of range are refused on load.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{Project, ProjectSettings};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_types::alert_rules::{AlertRuleError, AlertRuleSetting, HealthAlertRule};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectId, SampleFormat,
    SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-persistence-alert-rules-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn project() -> Project {
    let frame_rate = FrameRate::new(30, 1).unwrap();
    Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Alerts",
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

fn save(store: &ProjectStore, project: Project) {
    store
        .save(
            &StoredProject::from_project(
                project,
                RuntimeRouting::default(),
                ProjectPosition::default(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
}

#[test]
fn alert_rule_settings_round_trip_exactly_and_strictly() {
    let directory = TestDirectory::new("round-trip");
    let store = ProjectStore::new(directory.path("show.freemix")).unwrap();
    let manifest = directory.path("show.freemix/project.json");

    // No settings: the list is omitted.
    let empty = project();
    save(&store, empty.clone());
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(!text.contains("\"alert_rules\""), "{text}");
    assert_eq!(store.load().unwrap().project(), &empty);

    let mut expected = project();
    let off = AlertRuleSetting {
        enabled: false,
        ..AlertRuleSetting::default_for(HealthAlertRule::OutputFailed)
    };
    let cpu = AlertRuleSetting {
        trigger_milli: 800,
        clear_milli: 600,
        hold_ms: 5_000,
        ..AlertRuleSetting::default_for(HealthAlertRule::CpuSaturated)
    };
    // Set out of order: the project keeps rule order.
    expected.set_alert_rule(off).unwrap();
    assert_eq!(
        expected.set_alert_rule(cpu),
        Ok(AlertRuleSetting::default_for(HealthAlertRule::CpuSaturated))
    );
    // A rule set back to its default is no longer kept.
    let gpu = AlertRuleSetting {
        hold_ms: 1_000,
        ..AlertRuleSetting::default_for(HealthAlertRule::GpuSaturated)
    };
    expected.set_alert_rule(gpu).unwrap();
    expected
        .set_alert_rule(AlertRuleSetting::default_for(HealthAlertRule::GpuSaturated))
        .unwrap();
    assert_eq!(expected.alert_rules(), [cpu, off]);
    // A setting without a band changes nothing.
    assert_eq!(
        expected.set_alert_rule(AlertRuleSetting {
            clear_milli: 900,
            ..cpu
        }),
        Err(AlertRuleError::Hysteresis)
    );
    assert_eq!(expected.alert_rules(), [cpu, off]);

    save(&store, expected.clone());
    assert_eq!(store.load().unwrap().project(), &expected);
    let text = fs::read_to_string(&manifest).unwrap();
    let rules = "\"alert_rules\": [{\"rule\": \"cpu_saturated\", \"enabled\": true, \"trigger\": \
                 800, \"clear\": 600, \"hold_ms\": 5000}, {\"rule\": \"output_failed\", \
                 \"enabled\": false, \"trigger\": 1000, \"clear\": 0, \"hold_ms\": 0}]";
    assert!(text.contains(rules), "{text}");

    for (from, to) in [
        // Syntax: every field is required, nothing else is accepted, and
        // the rule must be known.
        ("\"hold_ms\": 5000", "\"hold_ms\": 5000, \"severity\": 1"),
        (", \"clear\": 600", ""),
        ("\"cpu_saturated\"", "\"disk_full\""),
        // Validation: out of range, no band, too long a hold, a repeated
        // rule, and a rule at its default.
        ("\"trigger\": 800", "\"trigger\": 1001"),
        ("\"clear\": 600", "\"clear\": 800"),
        ("\"hold_ms\": 5000", "\"hold_ms\": 3600001"),
        ("\"output_failed\"", "\"cpu_saturated\""),
        (
            "\"enabled\": false, \"trigger\": 1000",
            "\"enabled\": true, \"trigger\": 1000",
        ),
    ] {
        assert!(text.contains(from), "{from}");
        fs::write(&manifest, text.replacen(from, to, 1)).unwrap();
        assert!(store.load().is_err(), "{from} -> {to} loaded");
    }
}
