use std::num::NonZeroU128;

use fm_scopes::{
    BinDimension, ColorMatrix, ColorMetadata, ColorRange, FalseColorBand, MAX_CADENCE_FRAMES,
    MIN_CADENCE_FRAMES, MonitorError, MonitorScopeKind, MonitorScopeSelection, MonitoringPoint,
    ScopeError, ScopeMonitor, ScopeMonitorConfig, ScopeSplitView, SplitOrientation, WaveformConfig,
    false_color_band, histogram, luma_waveform, rgb_parade, vectorscope,
};
use fm_types::InputId;
use fm_video::{ImageFrame, Rgba8};

const FULL_601: ColorMetadata = ColorMetadata::new(ColorMatrix::Bt601, ColorRange::Full);

fn frame(width: u32, pixels: &[Rgba8]) -> ImageFrame {
    let bytes = pixels
        .iter()
        .flat_map(|pixel| pixel.to_bytes())
        .collect::<Vec<_>>();
    ImageFrame::new(width, 1, usize::try_from(width).unwrap() * 4, bytes).unwrap()
}

fn sample_frame() -> ImageFrame {
    let pixels = (0..32_u8)
        .map(|code| Rgba8::new(code.wrapping_mul(7), 255 - code.wrapping_mul(3), code, 255))
        .collect::<Vec<_>>();
    frame(32, &pixels)
}

#[test]
fn default_config_is_valid() {
    assert!(ScopeMonitor::new(ScopeMonitorConfig::default()).is_ok());
}

#[test]
fn cadence_analyses_every_fifth_frame() {
    let frame = sample_frame();
    let monitor = ScopeMonitor::new(ScopeMonitorConfig {
        cadence_frames: 5,
        ..ScopeMonitorConfig::default()
    })
    .unwrap();

    for frame_index in 0..=10_u64 {
        let report = monitor.analyse(frame_index, &frame);
        if frame_index % 5 == 0 {
            assert_eq!(report.unwrap().frame_index(), frame_index);
        } else {
            assert!(report.is_none(), "frame {frame_index} must be skipped");
        }
    }
}

#[test]
fn report_scope_results_equal_direct_calls() {
    let frame = sample_frame();
    let config = ScopeMonitorConfig::default();
    let monitor = ScopeMonitor::new(config.clone()).unwrap();
    let report = monitor.analyse(0, &frame).unwrap();

    assert_eq!(
        report.waveform(),
        Some(&luma_waveform(&frame, config.waveform).unwrap())
    );
    assert_eq!(
        report.parade(),
        Some(&rgb_parade(&frame, config.parade).unwrap())
    );
    assert_eq!(
        report.vectorscope(),
        Some(&vectorscope(&frame, config.vectorscope).unwrap())
    );
    assert_eq!(
        report.histogram(),
        Some(&histogram(&frame, config.histogram).unwrap())
    );
    assert_eq!(report.point(), MonitoringPoint::Program);
    assert_eq!(report.split(), None);
}

#[test]
fn selection_keeps_insertion_order_and_deduplicates() {
    assert_eq!(MonitorScopeSelection::DEFAULT, MonitorScopeSelection::all());
    assert_eq!(
        MonitorScopeSelection::default(),
        MonitorScopeSelection::all()
    );
    assert_eq!(
        MonitorScopeSelection::all().iter().collect::<Vec<_>>(),
        MonitorScopeKind::ALL
    );

    let mut selection = MonitorScopeSelection::none();
    assert!(selection.is_empty());
    assert!(selection.insert(MonitorScopeKind::Histogram));
    assert!(selection.insert(MonitorScopeKind::Waveform));
    assert!(!selection.insert(MonitorScopeKind::Histogram));
    assert_eq!(selection.len(), 2);
    assert_eq!(
        selection.iter().collect::<Vec<_>>(),
        [MonitorScopeKind::Histogram, MonitorScopeKind::Waveform]
    );
    assert!(selection.contains(MonitorScopeKind::Histogram));
    assert!(selection.contains(MonitorScopeKind::Waveform));
    assert!(!selection.contains(MonitorScopeKind::Parade));

    assert!(selection.insert(MonitorScopeKind::Parade));
    assert!(selection.insert(MonitorScopeKind::Vectorscope));
    assert_eq!(selection.len(), 4);
    assert!(!selection.insert(MonitorScopeKind::Vectorscope));
    assert_eq!(
        selection.iter().collect::<Vec<_>>(),
        [
            MonitorScopeKind::Histogram,
            MonitorScopeKind::Waveform,
            MonitorScopeKind::Parade,
            MonitorScopeKind::Vectorscope,
        ]
    );

    let mut iterated = Vec::new();
    for kind in &selection {
        iterated.push(kind);
    }
    assert_eq!(iterated, selection.iter().collect::<Vec<_>>());
}

#[test]
fn report_presence_follows_selection() {
    let frame = sample_frame();
    let mut scopes = MonitorScopeSelection::none();
    assert!(scopes.insert(MonitorScopeKind::Parade));
    let monitor = ScopeMonitor::new(ScopeMonitorConfig {
        scopes,
        ..ScopeMonitorConfig::default()
    })
    .unwrap();
    let report = monitor.analyse(0, &frame).unwrap();
    assert!(report.waveform().is_none());
    assert!(report.parade().is_some());
    assert!(report.vectorscope().is_none());
    assert!(report.histogram().is_none());

    let all = ScopeMonitor::new(ScopeMonitorConfig::default())
        .unwrap()
        .analyse(0, &frame)
        .unwrap();
    assert!(all.waveform().is_some());
    assert!(all.parade().is_some());
    assert!(all.vectorscope().is_some());
    assert!(all.histogram().is_some());
}

#[test]
fn input_point_preserves_full_width_identity() {
    let value = NonZeroU128::new((u128::from(u64::MAX) << 64) | 0x0123_4567_89ab_cdef).unwrap();
    let input = InputId::new(value);
    let point = MonitoringPoint::Input(input);

    let mut monitor = ScopeMonitor::new(ScopeMonitorConfig {
        point,
        ..ScopeMonitorConfig::default()
    })
    .unwrap();
    assert_eq!(monitor.point(), point);
    match monitor.point() {
        MonitoringPoint::Input(monitored) => assert_eq!(monitored.get(), value),
        other => panic!("expected an input point, got {other:?}"),
    }
    assert_eq!(monitor.analyse(0, &sample_frame()).unwrap().point(), point);

    assert_ne!(MonitoringPoint::Program, MonitoringPoint::Preview);
    assert_ne!(MonitoringPoint::Program, point);
    assert_ne!(MonitoringPoint::Preview, point);

    monitor.set_point(MonitoringPoint::Preview);
    assert_eq!(monitor.point(), MonitoringPoint::Preview);
    assert_eq!(
        monitor.analyse(0, &sample_frame()).unwrap().point(),
        MonitoringPoint::Preview
    );
}

#[test]
fn split_view_requires_parts_that_fill_the_surface() {
    assert!(ScopeSplitView::new(SplitOrientation::Horizontal, 0, 10_000).is_err());
    assert!(ScopeSplitView::new(SplitOrientation::Vertical, 10_000, 0).is_err());
    assert!(ScopeSplitView::new(SplitOrientation::Horizontal, 0, 0).is_err());
    assert!(ScopeSplitView::new(SplitOrientation::Horizontal, 4_000, 5_000).is_err());
    assert!(ScopeSplitView::new(SplitOrientation::Horizontal, 6_000, 6_000).is_err());
    assert!(ScopeSplitView::new(SplitOrientation::Horizontal, 9_999, 1).is_ok());
    assert!(ScopeSplitView::new(SplitOrientation::Horizontal, 1_234, 8_766).is_ok());

    let view = ScopeSplitView::new(SplitOrientation::Vertical, 7_500, 2_500).unwrap();
    assert_eq!(view.orientation(), SplitOrientation::Vertical);
    assert_eq!(view.video_basis_points(), 7_500);
    assert_eq!(view.scope_basis_points(), 2_500);

    let monitor = ScopeMonitor::new(ScopeMonitorConfig {
        split: Some(view),
        ..ScopeMonitorConfig::default()
    })
    .unwrap();
    assert_eq!(monitor.split(), Some(view));
    assert_eq!(
        monitor.analyse(0, &sample_frame()).unwrap().split(),
        Some(view)
    );
}

#[test]
fn cadence_bounds_and_scope_configs_are_validated() {
    assert_eq!((MIN_CADENCE_FRAMES, MAX_CADENCE_FRAMES), (1, 3_600));
    for cadence in [0, MAX_CADENCE_FRAMES + 1] {
        assert!(
            ScopeMonitor::new(ScopeMonitorConfig {
                cadence_frames: cadence,
                ..ScopeMonitorConfig::default()
            })
            .is_err()
        );
    }
    for cadence in [MIN_CADENCE_FRAMES, MAX_CADENCE_FRAMES] {
        assert!(
            ScopeMonitor::new(ScopeMonitorConfig {
                cadence_frames: cadence,
                ..ScopeMonitorConfig::default()
            })
            .is_ok()
        );
    }

    let mut monitor = ScopeMonitor::new(ScopeMonitorConfig::default()).unwrap();
    assert!(monitor.set_cadence_frames(0).is_err());
    assert!(monitor.set_cadence_frames(MAX_CADENCE_FRAMES + 1).is_err());
    assert_eq!(monitor.cadence_frames(), MIN_CADENCE_FRAMES);
    assert!(monitor.set_cadence_frames(30).is_ok());
    assert_eq!(monitor.cadence_frames(), 30);

    let invalid = ScopeMonitor::new(ScopeMonitorConfig {
        waveform: WaveformConfig {
            horizontal_bins: 0,
            ..ScopeMonitorConfig::default().waveform
        },
        ..ScopeMonitorConfig::default()
    });
    assert_eq!(
        invalid.unwrap_err(),
        MonitorError::Scope(ScopeError::ZeroBins {
            dimension: BinDimension::Horizontal,
        })
    );
}

#[test]
fn false_color_palette_covers_every_luma_code_once() {
    let mut next = 0_u16;
    let mut names = Vec::new();
    for (index, band) in FalseColorBand::ALL.into_iter().enumerate() {
        let range = band.range();
        let start = *range.start();
        let end = *range.end();
        assert_eq!(u16::from(start), next, "bands must tile the code space");
        assert_eq!(band.code(), index, "palette codes must follow band order");
        for code in range {
            assert_eq!(false_color_band(code), band, "code {code} maps to {band:?}");
        }
        assert!(!names.contains(&band.name()));
        names.push(band.name());
        next = u16::from(end) + 1;
    }
    assert_eq!(next, 256);

    assert_eq!(false_color_band(0), FalseColorBand::Purple);
    assert_eq!(false_color_band(15), FalseColorBand::Purple);
    assert_eq!(false_color_band(16), FalseColorBand::Blue);
    assert_eq!(false_color_band(63), FalseColorBand::Blue);
    assert_eq!(false_color_band(64), FalseColorBand::Green);
    assert_eq!(false_color_band(127), FalseColorBand::Green);
    assert_eq!(false_color_band(128), FalseColorBand::Yellow);
    assert_eq!(false_color_band(191), FalseColorBand::Yellow);
    assert_eq!(false_color_band(192), FalseColorBand::Orange);
    assert_eq!(false_color_band(234), FalseColorBand::Orange);
    assert_eq!(false_color_band(235), FalseColorBand::Red);
    assert_eq!(false_color_band(255), FalseColorBand::Red);
    assert_eq!(FalseColorBand::COUNT, FalseColorBand::ALL.len());
}

#[test]
fn report_false_color_counts_match_the_frame() {
    let pixels = (0..=u8::MAX)
        .map(|code| Rgba8::new(code, code, code, u8::MAX))
        .collect::<Vec<_>>();
    let frame = frame(256, &pixels);
    let mut config = ScopeMonitorConfig::default();
    config.waveform.color = FULL_601;
    let monitor = ScopeMonitor::new(config).unwrap();
    let report = monitor.analyse(0, &frame).unwrap();
    let counts = report.false_color().unwrap();

    assert_eq!(counts.total(), 256);
    assert_eq!(counts.counts().iter().sum::<u64>(), 256);
    for band in FalseColorBand::ALL {
        assert_eq!(
            counts.count(band),
            u64::try_from(band.range().count()).unwrap(),
            "band {band:?}"
        );
    }
}

#[test]
fn analysis_is_deterministic() {
    let frame = sample_frame();
    let monitor = ScopeMonitor::new(ScopeMonitorConfig::default()).unwrap();
    let first = monitor.analyse(0, &frame);
    let second = monitor.analyse(0, &frame);
    assert_eq!(first, second);
    assert!(first.is_some());

    let gated = ScopeMonitor::new(ScopeMonitorConfig {
        cadence_frames: 5,
        ..ScopeMonitorConfig::default()
    })
    .unwrap();
    assert_eq!(gated.analyse(7, &frame), None);
    assert!(gated.analyse(5, &frame).is_some());
}
