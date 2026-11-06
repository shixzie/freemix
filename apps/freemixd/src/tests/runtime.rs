/// A recorder that refuses to start after its sessions were created must
/// not leave the video or WAV session behind: here an out-of-range
/// bitrate makes the real recorder refuse, and the next start may reuse
/// the same name. A stale WAV sidecar never reaches a start: it takes
/// the name.
#[cfg(feature = "native-media")]
#[test]
fn refused_recorder_start_removes_the_created_session() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("show.mp4");
    let wav = directory.path().join("show.wav");
    let mut config = RecordConfig::new(
        RecordFormat::new(
            64,
            36,
            fm_types::FrameRate::new(30, 1).unwrap(),
            fm_types::SampleRate::new(48_000).unwrap(),
            fm_types::ChannelLayout::stereo(),
            SequenceNumber::new(0),
        )
        .unwrap(),
    );
    config.video_bitrate_kbps = Some(1);
    let names = recording_session::SegmentNames::new(None);
    let parent = record_output_parent(&path).unwrap();
    let planned = recording_session::plan(&path, &parent, &names, 1, 1_000_000, Some(1)).unwrap();

    assert!(
        start_session_recorder(planned, config).is_err(),
        "an invalid bitrate refuses the recorder start"
    );
    assert!(!path.exists(), "the refused start left its segment behind");
    assert!(!wav.exists(), "the refused start left its sidecar behind");
    for manifest in ["show.fmsession", "show.wav.fmsession"] {
        assert!(
            !directory.path().join(manifest).exists(),
            "the refused start left {manifest} behind"
        );
    }
    assert!(!names.taken(&path), "the refused name is free again");

    fs::write(&wav, b"stale").unwrap();
    assert!(names.taken(&path), "a stale sidecar takes the name");
    assert_eq!(
        next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
        directory.path().join("show-002.mp4")
    );
    assert_eq!(fs::read(wav).unwrap(), b"stale");
}

#[cfg(feature = "native-media")]
#[test]
fn recording_output_is_exclusive_and_never_overwrites() {
    let directory = tempfile::tempdir().unwrap();
    let names = recording_session::SegmentNames::new(None);
    let path = directory.path().join("capture.mp4");
    fs::write(&path, b"keep-me").unwrap();
    // Name derivation skips an existing file...
    assert!(names.taken(&path));
    assert_eq!(
        next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
        directory.path().join("capture-002.mp4")
    );
    // ...and a session forced onto it never overwrites: segment creation
    // is exclusive, so the session moves on to its next segment name.
    let opened = recording_session::open(planned_session(&path, &names)).unwrap();
    drop(opened.output);
    let (status, config) = (&opened.status, opened.status.config().clone());
    assert_eq!(status.get().video.segments.len(), 0);
    let second = fm_record::RecordingSession::start(
        config.clone(),
        fm_record::FileStorage::new(fm_record::UnknownSpace),
    );
    assert!(
        matches!(
            second,
            Err(fm_record::SessionError::ManifestExists { root: 0 })
        ),
        "a second session of the same name is refused while its manifest exists"
    );
    recording_session::discard_unused(status);
    let mut session = fm_record::RecordingSession::start(
        config,
        fm_record::FileStorage::new(fm_record::UnknownSpace),
    )
    .unwrap();
    let opened = session.open_segment().unwrap();
    assert_eq!(opened.index, 2);
    assert_eq!(
        opened.path,
        fs::canonicalize(directory.path())
            .unwrap()
            .join("capture-0002.mp4")
    );
    assert_eq!(fs::read(&path).unwrap(), b"keep-me");

    let occupied_directory = directory.path().join("occupied.mp4");
    fs::create_dir(&occupied_directory).unwrap();
    assert!(names.taken(&occupied_directory));
}

#[cfg(all(feature = "native-media", unix))]
#[test]
fn recording_output_rejects_existing_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target");
    fs::write(&target, b"keep-me").unwrap();
    let output = directory.path().join("capture.mp4");
    symlink(&target, &output).unwrap();
    let names = recording_session::SegmentNames::new(None);
    assert!(names.taken(&output));
    let opened = recording_session::open(planned_session(&output, &names)).unwrap();
    drop(opened.output);
    recording_session::discard_unused(&opened.status);
    let mut session = fm_record::RecordingSession::start(
        opened.status.config().clone(),
        fm_record::FileStorage::new(fm_record::UnknownSpace),
    )
    .unwrap();
    let opened = session.open_segment().unwrap();
    assert_ne!(
        opened.path.file_name(),
        output.file_name(),
        "the symlinked name is never opened"
    );
    assert_eq!(fs::read(target).unwrap(), b"keep-me");
}

#[cfg(feature = "native-media")]
#[test]
fn a_started_session_reserves_its_name_until_its_manifest_exists() {
    let directory = tempfile::tempdir().unwrap();
    let names = recording_session::SegmentNames::new(None);
    let path = directory.path().join("capture.mp4");
    let planned = planned_session(&path, &names);
    assert!(names.taken(&path), "planning reserves the name");
    assert_eq!(
        next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
        directory.path().join("capture-002.mp4")
    );
    let sink = recording_session::open(planned).unwrap().output;
    assert!(names.taken(&path), "the manifest keeps the name taken");
    drop(sink);
    assert!(names.taken(&path), "a stopped session keeps its name");
}

#[test]
fn parses_fullscreen_program_display_opt_in() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--fullscreen-display",
            "2",
        ]))
        .unwrap(),
        Command::Serve {
            native_media: true,
            fullscreen_program: true,
            display_outputs,
            display_inventory: None,
            ..
        } if display_outputs == [DisplayOutputConfig::program_on(DisplaySelector::Index(2))]
    ));
}

#[test]
fn fullscreen_display_defaults_to_zero() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
        ]))
        .unwrap(),
        Command::Serve {
            fullscreen_program: true,
            display_outputs,
            ..
        } if display_outputs == [DisplayOutputConfig::program_on(DisplaySelector::Index(0))]
    ));
}

#[test]
fn parses_two_independent_display_outputs() {
    let Command::Serve {
        fullscreen_program,
        display_outputs,
        display_inventory,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--display-output",
        "display=name:Left,source=program,scaling=fit",
        "--display-output",
        "display=id:x:66,source=output:2,mode=1920x1080@60,scaling=1:1,hdr=auto",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert!(fullscreen_program);
    assert_eq!(display_inventory, None);
    assert_eq!(display_outputs.len(), 2);
    assert_eq!(
        display_outputs[1].source,
        fm_io_api::display::DisplayOutputSource::Output(fm_types::OutputId::new(
            NonZeroU128::new(2).unwrap()
        ))
    );
    assert_eq!(
        display_outputs[1].scaling,
        fm_types::OutputScaling::OneToOne
    );

    let Command::Serve {
        fullscreen_program,
        display_inventory,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--display-output",
        "display=0",
        "--diagnostic-display-inventory",
        "displays.tsv",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert!(!fullscreen_program, "the inventory host is headless");
    assert_eq!(display_inventory, Some(PathBuf::from("displays.tsv")));
}

#[test]
fn diagnostic_capture_inventory_requires_native_media_and_one_file() {
    let Command::Serve {
        capture_inventory, ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--diagnostic-capture-inventory",
        "captures.tsv",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert_eq!(capture_inventory, Some(PathBuf::from("captures.tsv")));
    for arguments in [
        &[
            "serve",
            "show.freemix",
            "--diagnostic-capture-inventory",
            "a",
        ][..],
        &[
            "serve",
            "show.freemix",
            "--native-media",
            "--diagnostic-capture-inventory",
            "a",
            "--diagnostic-capture-inventory",
            "b",
        ][..],
        &[
            "serve",
            "show.freemix",
            "--native-media",
            "--diagnostic-capture-inventory",
        ][..],
    ] {
        assert!(parse_args(strings(arguments)).is_err(), "{arguments:?}");
    }
}

#[test]
fn display_presenter_if_available_follows_the_build() {
    let Command::Serve {
        native_media,
        fullscreen_program,
        presenter_fallback,
        display_outputs,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--display-presenter-if-available",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    let surfaces = cfg!(feature = "display-surface");
    assert!(!native_media, "without a presenter it serves simulated");
    assert_eq!(fullscreen_program, surfaces, "the presenter opens windows");
    assert_eq!(presenter_fallback, surfaces);
    assert!(display_outputs.is_empty(), "the project's slots are kept");

    // An explicit presenter or diagnostic inventory keeps its own
    // behavior, and options the windowed daemon does not serve keep the
    // daemon headless.
    for options in [
        &["--native-media", "--diagnostic-display-inventory", "d.tsv"][..],
        &["--status-listen", "127.0.0.1:0"][..],
    ] {
        let mut arguments = vec!["serve", "show.freemix", "--display-presenter-if-available"];
        arguments.extend(options);
        let Command::Serve {
            fullscreen_program,
            presenter_fallback,
            ..
        } = parse_args(strings(&arguments)).unwrap()
        else {
            panic!("expected serve");
        };
        assert!(!fullscreen_program, "{options:?}");
        assert!(!presenter_fallback, "{options:?}");
    }
    let error = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--display-presenter-if-available",
        "--display-presenter-if-available",
    ]))
    .unwrap_err();
    assert!(error.to_string().contains("duplicate option"), "{error}");

    // The web gateway rides along with the presenter: Studio's
    // supervised daemon serves phones and the program windows at once.
    let Command::Serve {
        fullscreen_program,
        presenter_fallback,
        web_listen,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--display-presenter-if-available",
        "--web-listen",
        "0.0.0.0:0",
        "--web-tls",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert_eq!(fullscreen_program, surfaces);
    assert_eq!(presenter_fallback, surfaces);
    assert!(web_listen.is_some());
}

#[test]
fn display_presenter_opens_the_project_slots_in_windows() {
    let Command::Serve {
        fullscreen_program,
        display_outputs,
        display_inventory,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--display-presenter",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert!(fullscreen_program, "the presenter opens real windows");
    assert!(display_outputs.is_empty(), "the project's slots are kept");
    assert_eq!(display_inventory, None);

    // The diagnostic inventory host presents the project's slots alone.
    let Command::Serve {
        fullscreen_program,
        display_outputs,
        ..
    } = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--diagnostic-display-inventory",
        "displays.tsv",
    ]))
    .unwrap()
    else {
        panic!("expected serve");
    };
    assert!(!fullscreen_program);
    assert_eq!(display_outputs, Vec::new());
}

#[test]
fn display_output_overrides_replace_the_project_slots_on_load() {
    let slot = |number| DisplayOutputSlot::new(number).unwrap();
    let stored = test_project();
    let mut project = stored.project().clone();
    project
        .set_display_output(slot(2), "display=index:4".parse().unwrap())
        .unwrap();
    let stored = stored.with_project(project).unwrap();
    let unchanged = apply_display_output_overrides(stored.clone(), &[]).unwrap();
    assert_eq!(unchanged.project().display_outputs().len(), 1);

    let overrides = ["display=index:0,source=preview"
        .parse::<DisplayOutputConfig>()
        .unwrap()];
    let replaced = apply_display_output_overrides(stored.clone(), &overrides).unwrap();
    assert_eq!(replaced.project().display_outputs().len(), 1);
    assert_eq!(
        replaced.project().display_output(slot(1)),
        Some(&overrides[0])
    );
    assert_eq!(replaced.project().display_output(slot(2)), None);

    let unknown = ["display=index:0,source=output:4242".parse().unwrap()];
    let error = apply_display_output_overrides(stored, &unknown).unwrap_err();
    assert!(
        error.to_string().contains("output 4242 does not exist"),
        "{error}"
    );
}

/// The journal times every append it makes durable, so a native daemon
/// knows how much of a frame interval a command's sync needs; it
/// reports nothing before its first append.
#[test]
fn the_journal_reports_how_long_its_recent_appends_took() {
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    store.save(&test_project()).unwrap();
    let mut durable = load_and_recover(&store, false).unwrap();
    let journal = DurableJournal::open(&store).unwrap();
    assert_eq!(journal.recent_append_peak(), None);
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let execution = execute_durable_command(
        &mut control,
        &journal,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "wall",
            "wall-key",
            CommandPayload::SetDisplayOutput {
                slot: DisplayOutputSlot::new(1).unwrap(),
                config: Box::new("display=name:Wall".parse().unwrap()),
            },
        ),
        0,
    )
    .unwrap();
    assert!(
        matches!(
            execution.submission.output.result,
            CommandResult::Accepted { .. }
        ),
        "{:?}",
        execution.submission.output.result
    );
    assert!(journal.recent_append_peak().is_some());
}

/// `--display-output` overrides reach the manifest before any command
/// is journaled, so an edit accepted against the overridden slots
/// replays after a crash instead of conflicting with the old slots.
#[test]
fn display_output_overrides_are_durable_before_journaled_edits() {
    let slot = |number| DisplayOutputSlot::new(number).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
    let stored = test_project();
    let mut project = stored.project().clone();
    project
        .set_display_output(slot(1), "display=name:Wall".parse().unwrap())
        .unwrap();
    let stored = stored.with_project(project).unwrap();
    store.save(&stored).unwrap();

    let recovered = load_and_recover(&store, false).unwrap();
    let journal = DurableJournal::open(&store).unwrap();
    let overrides = ["display=index:0".parse::<DisplayOutputConfig>().unwrap()];
    let mut durable = persist_display_output_overrides(&journal, &recovered, &overrides).unwrap();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    // Slot 2 takes the display slot 1 named before the override.
    let execution = execute_durable_command(
        &mut control,
        &journal,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "wall",
            "wall-key",
            CommandPayload::SetDisplayOutput {
                slot: slot(2),
                config: Box::new("display=name:Wall".parse().unwrap()),
            },
        ),
        0,
    )
    .unwrap();
    assert!(
        matches!(
            execution.submission.output.result,
            CommandResult::Accepted { .. }
        ),
        "{:?}",
        execution.submission.output.result
    );
    drop(journal);

    // A crash here recovers from the manifest and the journal.
    let recovered = load_and_recover(&store, false).unwrap();
    assert_eq!(
        recovered.project().display_outputs(),
        durable.project().display_outputs()
    );
    assert_eq!(
        recovered.project().display_output(slot(1)),
        Some(&overrides[0])
    );
}

#[test]
fn rejects_invalid_display_output_combinations() {
    for (arguments, expected) in [
        (
            vec!["--display-output", "display=0"],
            "--display-output requires --native-media",
        ),
        (
            vec![
                "--native-media",
                "--display-output",
                "display=0,scaling=zoom",
            ],
            "invalid --display-output",
        ),
        (
            vec![
                "--native-media",
                "--display-output",
                "display=0",
                "--display-output",
                "display=1",
                "--display-output",
                "display=2",
            ],
            "at most 2",
        ),
        (
            vec![
                "--native-media",
                "--display-output",
                "display=0",
                "--display-output",
                "display=index:0,source=output:2",
            ],
            "same display",
        ),
        (
            vec![
                "--native-media",
                "--fullscreen-program",
                "--display-output",
                "display=1",
            ],
            "--display-output replaces --fullscreen-program",
        ),
        (
            vec!["--diagnostic-display-inventory", "d.tsv"],
            "--diagnostic-display-inventory requires --native-media",
        ),
        (
            vec!["--display-presenter"],
            "--display-presenter requires --native-media",
        ),
        (
            vec![
                "--native-media",
                "--display-presenter",
                "--diagnostic-display-inventory",
                "d.tsv",
            ],
            "cannot be combined with --fullscreen-program or --display-presenter",
        ),
        (
            vec![
                "--native-media",
                "--fullscreen-program",
                "--diagnostic-display-inventory",
                "d.tsv",
            ],
            "cannot be combined with --fullscreen-program",
        ),
        (
            vec![
                "--native-media",
                "--display-output",
                "display=0",
                "--status-listen",
                "127.0.0.1:0",
            ],
            "--status-listen cannot be combined with --fullscreen-program",
        ),
    ] {
        let mut full = strings(&["serve", "show.freemix"]);
        full.extend(arguments.iter().map(|value| (*value).to_owned()));
        let error = parse_args(full).unwrap_err().to_string();
        assert!(error.contains(expected), "{arguments:?}: {error}");
    }
}

#[test]
fn rejects_invalid_fullscreen_option_relationships_and_values() {
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--fullscreen-program"]),
            "--fullscreen-program requires --native-media",
        ),
        (
            strings(&["serve", "show.freemix", "--fullscreen-display", "0"]),
            "--fullscreen-display requires --fullscreen-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--fullscreen-display",
            ]),
            "missing value for --fullscreen-display",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
    assert!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--fullscreen-display",
            "-1",
        ]))
        .unwrap_err()
        .to_string()
        .contains("invalid fullscreen display index")
    );
}

#[test]
fn rejects_duplicate_fullscreen_options() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--fullscreen-program",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--fullscreen-display",
            "0",
            "--fullscreen-display",
            "1",
        ]),
    ] {
        assert!(parse_args(arguments).is_err());
    }
}

/// A Preview composite is rendered for a presenting display output that
/// shows Preview or for the Preview tap's capture, and for nothing else:
/// not for a tap frame a late loop paced out, however due display
/// outputs (none presenting) are.
#[cfg(feature = "native-media")]
#[test]
fn a_paced_out_preview_tap_frame_renders_no_preview_composite() {
    // No display shows Preview: only the tap's capture renders one.
    assert!(!super::preview_composite_due(false, true, false));
    assert!(super::preview_composite_due(false, true, true));
    assert!(super::preview_composite_due(false, false, true));
    // A display showing Preview renders it when it presents.
    assert!(super::preview_composite_due(true, true, false));
    assert!(!super::preview_composite_due(true, false, false));
}

/// A command dispatches while more of the frame interval is left than
/// recent journal appends took (half an interval before any was timed);
/// with less it waits for that frame, and once the frame was realized it
/// dispatches however little of the next interval is left, so it waits
/// at most one frame.
#[cfg(feature = "native-media")]
#[test]
fn a_command_waits_at_most_one_frame_for_room_before_its_deadline() {
    let open = |left: u64, peak: Option<u64>, frame: u64, deferred: &mut Option<FrameNumber>| {
        super::command_window_open(
            Duration::from_millis(left),
            Duration::from_millis(40),
            peak.map(Duration::from_millis),
            FrameNumber::new(frame),
            deferred,
        )
    };
    let mut deferred = None;
    // Untimed journal: half an interval is needed.
    assert!(open(20, None, 7, &mut deferred));
    assert_eq!(deferred, None);
    // Too little left: held back for frame 7, and again on a later pass
    // before frame 7 was realized.
    for left in [19, 3, 0] {
        assert!(!open(left, None, 7, &mut deferred));
        assert_eq!(deferred, Some(FrameNumber::new(7)));
    }
    // Frame 7 was realized: it goes now, even with little of frame 8's
    // interval left (a slow realization), and the hold is cleared.
    assert!(open(5, None, 8, &mut deferred));
    assert_eq!(deferred, None);
    // A quick journal needs only its peak and the staging margin.
    assert!(open(4, Some(1), 9, &mut deferred));
    assert!(!open(2, Some(1), 9, &mut deferred));
    assert_eq!(deferred, Some(FrameNumber::new(9)));
    // Room enough clears a stale hold too.
    assert!(open(30, Some(1), 9, &mut deferred));
    assert_eq!(deferred, None);
    // An overdue frame is realized first, whatever the journal.
    assert!(!open(0, Some(0), 10, &mut deferred));
    // A journal slower than a frame interval: a command always takes a
    // frame boundary, right after the frame it waited for.
    deferred = None;
    assert!(!open(39, Some(80), 11, &mut deferred));
    assert!(open(1, Some(80), 12, &mut deferred));
}

#[cfg(not(feature = "native-media"))]
#[test]
fn native_media_opt_in_fails_when_support_is_not_compiled() {
    let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
    let error = NativeDaemon::start(&store, &test_project(), None)
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "native-media support was not compiled in"
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_mode_rejects_unimplemented_simulated_sine_audio() {
    let error = validate_native_audio_modes(&test_project()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires unsupported simulated sine audio")
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_source_extraction_builds_retained_generators_without_assets() {
    let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
    let resolution = resolve_native_sources(&store, &test_project(), None).unwrap();
    let sources = resolution.sources;
    assert_eq!(sources.len(), 2);
    for (index, source) in sources.iter().enumerate() {
        let NativeResolvedSource::RetainedFrame { input, frame } = source else {
            panic!("simulated source must be retained")
        };
        assert_eq!(*input, test_input_id(u128::try_from(index + 1).unwrap()));
        assert_eq!(frame.payload().dimensions().width(), 1_280);
        assert_eq!(frame.payload().dimensions().height(), 720);
        assert_eq!(frame.timing().sequence().get(), 0);
        assert_eq!(frame.timing().presentation_timestamp().as_nanos(), 0);
        assert!(frame.metadata().is_some());
    }
    let NativeResolvedSource::RetainedFrame { frame, .. } = &sources[0] else {
        unreachable!()
    };
    assert_eq!(
        &frame.payload().plane(0).unwrap().bytes()[..4],
        &[7, 11, 13, 255]
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_source_extraction_keeps_scene_inputs_out_of_physical_sources() {
    let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
    let resolution = resolve_native_sources(&store, &scene_test_project(), None).unwrap();

    assert_eq!(resolution.sources.len(), 1);
    assert_eq!(resolution.sources[0].input(), test_input_id(2));
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn macos_camera_resolution_is_exact_non_prompting_and_reaps_helper() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");

    let directory = tempfile::tempdir().unwrap();
    let helper = fake_camera_helper(directory.path(), 0);
    let capture_marker = PathBuf::from(format!("{}.capture", helper.display()));
    let pid_file = PathBuf::from(format!("{}.pid", helper.display()));
    let permission_marker = PathBuf::from(format!("{}.permission", helper.display()));
    let (sources, mut cameras) =
        resolve_macos_camera_sources(&camera_test_project(&stable_key), Some(helper.as_path()))
            .unwrap();
    assert_eq!(sources.len(), 1);
    let NativeResolvedSource::LiveFrame { input, frame } = &sources[0] else {
        panic!("camera source must enter the live frame lane")
    };
    assert_eq!(*input, test_input_id(1));
    assert_eq!(frame.timing().sequence().get(), 7);
    assert_eq!(
        frame.timing().presentation_timestamp().as_nanos(),
        1_000_000_000
    );
    assert_eq!(frame.payload().plane(0).unwrap().bytes(), &[3, 5, 7, 255]);
    assert_eq!(
        frame.metadata().unwrap().color().transfer,
        fm_types::TransferFunction::Bt709
    );
    let pending = cameras.source_telemetry()[0];
    assert_eq!(pending.frames_received, 1);
    assert_eq!(pending.frames_ingested, 0);
    assert_eq!(pending.preflight_depth, 1);
    assert_eq!(pending.preflight_discarded, 0);
    assert_eq!(pending.frames_received, pending.preflight_depth);
    drop(sources);
    cameras.discard_preflight_frames();
    let discarded = cameras.source_telemetry()[0];
    assert_eq!(discarded.preflight_depth, 0);
    assert_eq!(discarded.preflight_discarded, 1);
    assert_eq!(discarded.frames_received, discarded.preflight_discarded);
    assert_eq!(
        fs::read_to_string(&capture_marker).unwrap(),
        "capture\nfake-camera\n1\n1\n30000\n1001\n"
    );
    assert!(!permission_marker.exists());
    let pid = fs::read_to_string(&pid_file).unwrap();
    drop(cameras);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success()),
        "camera helper {pid} survived source cleanup"
    );

    let unknown_directory = tempfile::tempdir().unwrap();
    let unknown_helper = fake_camera_helper(unknown_directory.path(), 0);
    let unknown_capture = PathBuf::from(format!("{}.capture", unknown_helper.display()));
    let error = resolve_macos_camera_sources(
        &camera_test_project("macos.avfoundation.camera.v1.999"),
        Some(unknown_helper.as_path()),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("unknown camera stable key"));
    assert!(!unknown_capture.exists(), "unknown key invoked capture");

    let prompt_directory = tempfile::tempdir().unwrap();
    let prompt_helper = fake_camera_helper(prompt_directory.path(), 1);
    let prompt_capture = PathBuf::from(format!("{}.capture", prompt_helper.display()));
    let prompt_request = PathBuf::from(format!("{}.permission", prompt_helper.display()));
    let error = resolve_macos_camera_sources(
        &camera_test_project(&stable_key),
        Some(prompt_helper.as_path()),
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("camera permission is not granted")
    );
    assert!(
        !prompt_capture.exists(),
        "permission preflight invoked capture"
    );
    assert!(
        !prompt_request.exists(),
        "daemon requested camera permission"
    );
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn many_camera_startup_failure_uses_aggregate_cleanup_and_reaps_all_helpers() {
    const CAMERA_COUNT: usize = 16;

    let keys = (0..CAMERA_COUNT)
        .map(|index| {
            let source_id =
                deterministic_camera_id(CameraIdKind::Source, &format!("fake-camera-{index}"));
            format!("macos.avfoundation.camera.v1.{source_id}")
        })
        .collect::<Vec<_>>();
    let project = many_camera_test_project(&keys);
    let directory = tempfile::tempdir().unwrap();
    let helper = many_camera_startup_failure_helper(directory.path(), CAMERA_COUNT);
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));

    let started = Instant::now();
    let error = resolve_macos_camera_sources(&project, Some(helper.as_path()))
        .err()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(error.to_string().contains("failed to start"));
    assert_eq!(
        fs::read_to_string(&pid_log).unwrap().lines().count(),
        CAMERA_COUNT
    );
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_poll_into_ingests_recovered_mapped_discontinuity_while_ticks_continue() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = recovering_camera_helper(directory.path(), Some(3), 20);
    let capture_log = PathBuf::from(format!("{}.capture", helper.display()));
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let runtime = NativeMediaRuntime::new_blocking([platform_native_backend()]).unwrap();
    let (sources, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(3, Duration::from_millis(100)),
    )
    .unwrap();
    let NativeResolvedSource::LiveFrame { input, frame } = &sources[0] else {
        panic!("camera source must be live")
    };
    assert_eq!(*input, test_input_id(1));
    assert_eq!(frame.timing().sequence().get(), 7);
    let mut playback = runtime
        .preflight_resolved_source_playback_mixed_blocking(
            None,
            sources,
            native_clock_domain(),
            StreamSelector::Best,
            NativeSourceLimits::default(),
        )
        .unwrap();
    cameras.mark_preflight_frames_ingested();
    let committed = cameras.source_telemetry()[0];
    assert_eq!(committed.frames_ingested, 1);
    assert_eq!(committed.preflight_depth, 0);
    assert_eq!(committed.preflight_discarded, 0);
    assert_eq!(committed.frames_received, committed.accounted_frames());

    let mut control = test_control(&project);
    let server = test_server(&control);
    let mut rendered = 0_u64;
    let mut checkpointed = 0_u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    while cameras.inputs[0].ingested_frames == 1 && Instant::now() < deadline {
        cameras.poll_into(&runtime, &mut playback).unwrap();
        control.tick(&server).unwrap();
        rendered = rendered.saturating_add(1);
        let snapshot = control.idle_engine_snapshot().unwrap();
        stored_project_checkpoint(&project, &snapshot).unwrap();
        checkpointed = checkpointed.saturating_add(1);
        thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(cameras.inputs[0].ingested_frames, 2);
    assert_eq!(cameras.inputs[0].last_ingested_sequence, Some(8));
    assert!(cameras.inputs[0].last_ingested_discontinuity);
    assert!(rendered > 10, "render loop stalled during camera recovery");
    assert_eq!(checkpointed, rendered);
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_attempts, 2);
    assert_eq!(telemetry[0].recovery_successes, 1);
    assert_eq!(telemetry[0].recovery_exhausted, 0);
    assert_eq!(telemetry[0].recovery_worker_failures, 0);
    assert_eq!(
        telemetry[0].frames_received,
        telemetry[0].accounted_frames()
    );
    assert_eq!(
        telemetry[0].recovery_outcome,
        CameraRecoveryOutcome::Recovered
    );
    assert!(!telemetry[0].diagnostic().contains("fake-camera"));

    let captures = fs::read_to_string(capture_log).unwrap();
    let expected = ["capture", "fake-camera", "1", "1", "30000", "1001"];
    let captures = captures.lines().collect::<Vec<_>>();
    assert_eq!(captures.len(), 18);
    for invocation in captures.chunks_exact(expected.len()) {
        assert_eq!(invocation, expected);
    }

    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_recovery_rearms_after_exhaustion_and_eventually_recovers() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = recovering_camera_helper(directory.path(), Some(4), 20);
    let capture_count = PathBuf::from(format!("{}.count", helper.display()));
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(2, Duration::from_millis(20)),
    )
    .unwrap();

    let mut control = test_control(&project);
    let server = test_server(&control);
    let mut rendered = 0_u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cameras.poll_with(|_, _| Ok(())).unwrap();
        control.tick(&server).unwrap();
        rendered = rendered.saturating_add(1);
        let telemetry = cameras.source_telemetry();
        if telemetry[0].recovery_successes == 1 || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_attempts, 3);
    assert_eq!(telemetry[0].recovery_successes, 1);
    assert_eq!(telemetry[0].recovery_exhausted, 1);
    assert_eq!(telemetry[0].recovery_worker_failures, 0);
    assert!(
        rendered > 10,
        "render loop stopped on recoverable camera loss"
    );
    assert_eq!(fs::read_to_string(capture_count).unwrap(), "4");
    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn generic_camera_helper_exit_is_recoverable_but_malformed_contract_is_fatal() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = recovering_camera_helper(directory.path(), Some(2), 21);
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(2, Duration::from_millis(20)),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while cameras.inputs[0].ingested_frames == 0 && Instant::now() < deadline {
        let poll_started = Instant::now();
        cameras.poll_with(|_, _| Ok(())).unwrap();
        assert!(
            poll_started.elapsed() < Duration::from_millis(100),
            "render-thread camera poll performed blocking cleanup"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_attempts, 1);
    assert_eq!(telemetry[0].recovery_successes, 1);
    assert_eq!(telemetry[0].recovery_exhausted, 0);
    assert_eq!(telemetry[0].recovery_worker_failures, 0);
    assert!(!recoverable_camera_error(&IoError::MalformedTimestamp(
        fm_io_api::TimestampValidationError::OriginalTimestampOverflow,
    )));
    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn malformed_camera_frame_bytes_are_fatal_and_never_restart_helper() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = malformed_runtime_camera_helper(directory.path());
    let capture_count = PathBuf::from(format!("{}.count", helper.display()));
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(2, Duration::from_millis(20)),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let error = loop {
        match cameras.poll_with(|_, _| Ok(())) {
            Ok(()) => {}
            Err(error) => break error,
        }
        assert!(
            Instant::now() < deadline,
            "malformed frame bytes were swallowed"
        );
        thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(
        error.downcast_ref::<IoError>(),
        Some(IoError::AdapterFailure {
            remediation: None,
            ..
        })
    ));
    assert_eq!(fs::read_to_string(capture_count).unwrap(), "1");
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_attempts, 0);
    assert_eq!(telemetry[0].recovery_exhausted, 0);
    assert_eq!(telemetry[0].recovery_worker_failures, 1);
    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn fatal_camera_contract_error_propagates_without_counting_as_exhaustion() {
    let supervisor = Arc::new(Mutex::new(CameraSupervisorState {
        frame: CameraFrameSlot::default(),
        snapshot: CameraWorkerSnapshot {
            telemetry: CameraTelemetry::default(),
            lifecycle: LifecycleState::Lost,
            health: EndpointHealthState::Failed,
            recovery_attempts: 0,
            recovery_successes: 0,
            recovery_exhausted: 0,
            recovery_worker_failures: 1,
            recovery_outcome: CameraRecoveryOutcome::WorkerFailed,
        },
    }));
    let handle = thread::spawn(|| CameraWorkerResult {
        failure: Some(IoError::MalformedTimestamp(
            fm_io_api::TimestampValidationError::OriginalTimestampOverflow,
        )),
        cleanup_failure: None,
    });
    let mut cameras = NativeCameraInputs {
        inputs: vec![NativeCameraInput {
            input: test_input_id(1),
            source: None,
            worker: Some(NativeCameraWorker {
                handle: Some(handle),
                cancel: Arc::new(AtomicBool::new(false)),
            }),
            supervisor,
            recovery_policy: test_camera_recovery_policy(1, Duration::from_millis(10)),
            ingested_frames: 0,
            ingest_failed: 0,
            preflight_depth: 0,
            preflight_discarded: 0,
            last_ingested_sequence: None,
            last_ingested_discontinuity: false,
        }],
        telemetry_emitted: false,
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    let error = loop {
        if let Err(error) = cameras.poll_with(|_, _| Ok(())) {
            break error;
        }
        assert!(
            Instant::now() < deadline,
            "fatal camera error was swallowed"
        );
        thread::sleep(Duration::from_millis(1));
    };
    assert!(error.to_string().contains("malformed timestamp"));
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_exhausted, 0);
    assert_eq!(telemetry[0].recovery_worker_failures, 1);
    assert_eq!(
        telemetry[0].recovery_outcome,
        CameraRecoveryOutcome::WorkerFailed
    );
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn one_recovering_camera_does_not_interrupt_second_camera() {
    let first_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera-a");
    let second_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera-b");
    let first_key = format!("macos.avfoundation.camera.v1.{first_id}");
    let second_key = format!("macos.avfoundation.camera.v1.{second_id}");
    let project = two_camera_test_project(&first_key, &second_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = two_camera_helper(directory.path());
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(2, Duration::from_millis(20)),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut recovered_input = None;
    while recovered_input.is_none() && Instant::now() < deadline {
        cameras
            .poll_with(|input, frame| {
                if frame
                    .timing()
                    .flags()
                    .contains(fm_frame::MediaFlags::DISCONTINUITY)
                {
                    recovered_input = Some(input);
                }
                Ok(())
            })
            .unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(recovered_input, Some(test_input_id(1)));
    let telemetry = cameras.source_telemetry();
    assert_eq!(telemetry[0].recovery_successes, 1);
    assert_eq!(telemetry[0].recovery_worker_failures, 0);
    assert_eq!(telemetry[1].lifecycle, LifecycleState::Running);
    assert_eq!(telemetry[1].recovery_attempts, 0);
    assert_eq!(telemetry[1].recovery_worker_failures, 0);
    cameras.shutdown().unwrap();
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_shutdown_during_recovery_is_aggregate_bounded_and_reaps_helper() {
    let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
    let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
    let project = camera_test_project(&stable_key);
    let directory = tempfile::tempdir().unwrap();
    let helper = hanging_recovery_camera_helper(directory.path());
    let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
    let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
        &project,
        Some(helper.as_path()),
        test_camera_recovery_policy(3, Duration::from_millis(20)),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while cameras.source_telemetry()[0].recovery_attempts == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(cameras.source_telemetry()[0].recovery_attempts, 1);

    let shutdown_started = Instant::now();
    cameras.shutdown().unwrap();
    assert!(shutdown_started.elapsed() < Duration::from_secs(4));
    assert_helper_processes_reaped(&pid_log);
}

#[cfg(feature = "native-media")]
#[test]
fn native_source_extraction_keeps_asset_resolution_errors_path_free() {
    let store = ProjectStore::new("secret-native-project-marker.freemix").unwrap();
    let error = resolve_native_sources(
        &store,
        &media_test_project("asset://secret-uri-marker/../clip.mkv"),
        None,
    )
    .err()
    .unwrap();
    let message = error.to_string();
    assert!(message.contains("invalid project asset URI"));
    assert!(!message.contains("secret-native-project-marker"));
    assert!(!message.contains("secret-uri-marker"));
}

#[cfg(feature = "native-media")]
#[test]
fn native_source_extraction_routes_png_to_one_timed_retained_frame() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("still-project.freemix");
    let assets = project_path.join("assets");
    fs::create_dir_all(&assets).unwrap();
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&[1, 2, 3, 4], 1, 1, ExtendedColorType::Rgba8)
        .unwrap();
    fs::write(assets.join("still.data"), png).unwrap();

    let sources = resolve_native_sources(
        &ProjectStore::new(&project_path).unwrap(),
        &media_test_project("asset://still.data"),
        None,
    )
    .unwrap()
    .sources;
    assert_eq!(sources.len(), 2);
    for source in sources {
        let NativeResolvedSource::RetainedFrame { frame, .. } = source else {
            panic!("PNG signature must route to retained still decode")
        };
        // A still media file is fitted into the output raster.
        assert_eq!(
            frame.payload().dimensions(),
            VideoDimensions::new(1_280, 720).unwrap()
        );
        let plane = frame.payload().plane(0).unwrap();
        let centre = 360 * plane.stride() + 640 * 4;
        assert_eq!(&plane.bytes()[centre..centre + 4], &[1, 2, 3, 4]);
        assert_eq!(&plane.bytes()[..4], &[0, 0, 0, 0]);
        assert_eq!(frame.timing().presentation_timestamp().as_nanos(), 0);
        assert_eq!(frame.timing().duration().as_nanos(), 1);
        assert_eq!(frame.timing().sequence().get(), 0);
        assert_eq!(frame.timing().clock_domain(), native_clock_domain());
    }
}

#[cfg(feature = "native-media")]
#[test]
fn recognized_corrupt_png_does_not_fall_back_to_ffmpeg() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("corrupt-still.freemix");
    let assets = project_path.join("assets");
    fs::create_dir_all(&assets).unwrap();
    fs::write(assets.join("bad.bin"), b"\x89PNG\r\n\x1a\ncorrupt").unwrap();

    let error = resolve_native_sources(
        &ProjectStore::new(&project_path).unwrap(),
        &media_test_project("asset://bad.bin"),
        None,
    )
    .err()
    .unwrap();
    let message = error.to_string();
    assert!(message.contains("native still decode failed"));
    assert!(!message.contains("corrupt-still"));
    assert!(!message.contains("bad.bin"));
}

#[cfg(feature = "native-media")]
#[test]
fn host_pacer_offsets_use_exact_rational_deadlines() {
    let rate = FrameRate::new(30_000, 1_001).unwrap();
    let mut pacer = FramePacer::new(rate, 0);
    assert_eq!(host_deadline_offset(&pacer).unwrap(), Duration::ZERO);
    pacer.advance().unwrap();
    assert_eq!(host_deadline_offset(&pacer).unwrap().as_nanos(), 33_366_666);
    for _ in 1..30_000 {
        pacer.advance().unwrap();
    }
    assert_eq!(host_deadline_offset(&pacer).unwrap().as_secs(), 1_001);
}

/// A command that could not be made durable is refused, not acknowledged
/// and not half-applied: the engine keeps its revision, publishes nothing,
/// and installs no receipt, so the same command may simply be sent again.
#[test]
fn a_command_that_cannot_be_made_durable_is_refused() {
    let mut durable = test_project();
    let initial_engine = restore_engine(&durable).unwrap().snapshot().unwrap();
    let mut control = test_control(&durable);
    let subscription = control.subscribe().unwrap();
    let before_diagnostics = control.diagnostics();
    let before_snapshot = control.snapshot().clone();
    let server = test_server(&control);
    let command = test_command("failed-save", "failed-save-key", CommandPayload::Cut);

    let execution = execute_durable_command(
        &mut control,
        &FailingSaver,
        &mut durable,
        &operator(),
        &server,
        &command,
        0,
    )
    .expect("a durability failure is refused, not propagated");

    assert!(matches!(
        &execution.submission.output.result,
        CommandResult::Rejected {
            code,
            retryable: true,
            current_revision: 0,
            ..
        } if code == "unavailable"
    ));
    assert_eq!(execution.submission.output.events, []);
    assert!(execution.submission.accepted.is_none());
    assert!(!execution.submission.replayed);
    assert_eq!(execution.runtime_events, []);
    assert_eq!(durable, test_project());
    assert_eq!(control.diagnostics(), before_diagnostics);
    assert_eq!(control.snapshot(), &before_snapshot);
    assert_eq!(live_engine_snapshot(&mut control), initial_engine);
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
    let prepared = control
        .prepare_submit(&operator(), command, 0)
        .unwrap()
        .prepared()
        .expect("failed save must not install a receipt");
    prepared.abort();
}

/// Regression: a command whose staged result the project schema refuses
/// (here a blank idempotency key, which a receipt may not carry) stopped
/// the whole daemon. It is refused, nothing is recorded or committed, and
/// the next command runs normally.
#[test]
fn a_command_whose_result_cannot_be_persisted_is_refused_not_fatal() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let before_durable = durable.clone();
    let before_engine = live_engine_snapshot(&mut control);
    let subscription = control.subscribe().unwrap();
    let saver = CountingSaver::default();

    let refused = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command("blank-key", "   ", CommandPayload::Cut),
        0,
    )
    .expect("a command-caused staging failure refuses the command");

    assert!(matches!(
        &refused.submission.output.result,
        CommandResult::Rejected {
            id,
            code,
            retryable: false,
            current_revision: 0,
            ..
        } if id == "blank-key" && code == "invalid_command"
    ));
    assert!(refused.submission.accepted.is_none());
    assert_eq!(refused.submission.output.events, []);
    assert_eq!(refused.runtime_events, []);
    assert_eq!(saver.0.get(), 0, "nothing was journaled");
    assert_eq!(durable, before_durable);
    assert_eq!(live_engine_snapshot(&mut control), before_engine);
    assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));

    let accepted = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command("after", "after-key", CommandPayload::Cut),
        0,
    )
    .unwrap();
    assert!(matches!(
        accepted.submission.output.result,
        CommandResult::Accepted { revision: 1, .. }
    ));
    assert_eq!(saver.0.get(), 1);
}

/// Regression: persisting a Stinger slot whose realization had not caught
/// up stopped the daemon. It is a retryable conflict.
#[test]
fn a_divergent_stinger_slot_refuses_the_command_as_a_retryable_conflict() {
    let durable = test_project();
    let inputs = vec![test_input_id(1), test_input_id(2)];
    let mut desired =
        SwitcherState::new(inputs.clone(), test_input_id(1), test_input_id(2)).unwrap();
    let realized = SwitcherState::new(inputs, test_input_id(1), test_input_id(2)).unwrap();
    let slot = StingerSlotId::new(2).unwrap();
    desired
        .configure_stinger(
            slot,
            StingerDescriptor::new(
                test_input_id(2),
                false,
                5,
                StingerAudioPolicy::Muted,
                MissingMediaFallback::Cut,
            ),
        )
        .unwrap();
    let mut project = durable.project().clone();

    let error = sync_project_stingers(&mut project, &desired, &realized).unwrap_err();
    let refusal = staging_refusal(error).expect("divergence is not fatal");
    assert_eq!(refusal.code, RejectionCode::Conflict);
    assert!(refusal.retryable);
    assert!(refusal.message.contains("Stinger slot 2"));

    // Once realization matches, the slot persists.
    let mut settled = realized.clone();
    settled
        .configure_stinger(slot, *desired.stinger(slot).descriptor().unwrap())
        .unwrap();
    sync_project_stingers(&mut project, &desired, &settled).unwrap();
    assert_eq!(project.stingers().len(), 1);
}

/// The classification boundary: staging failures caused by the command or
/// by recoverable runtime state refuse it; failures that mean the daemon's
/// own state can no longer be trusted stay fatal.
#[test]
fn staging_failures_are_classified_into_refusals_and_fatal_errors() {
    let refused = |error: Box<dyn Error>| staging_refusal(error).unwrap();
    let fatal = |error: Box<dyn Error>| staging_refusal(error).unwrap_err().to_string();

    let in_flight = refused(
        fm_control::ControlError::Engine(fm_engine::EngineError::Snapshot(
            fm_engine::SnapshotError::WorkInFlight,
        ))
        .into(),
    );
    assert_eq!(
        (in_flight.code, in_flight.retryable),
        (RejectionCode::Conflict, true)
    );
    let unpersistable = refused(fm_persistence::ProjectValidationError::EmptyIdempotencyKey.into());
    assert_eq!(
        (unpersistable.code, unpersistable.retryable),
        (RejectionCode::InvalidCommand, false)
    );

    // An engine that cannot advance or holds stale authority.
    assert_ne!(
        fatal(fm_control::ControlError::Engine(fm_engine::EngineError::StalePreparation).into()),
        ""
    );
    assert_ne!(
        fatal(fm_control::ControlError::RuntimeSequenceExhausted.into()),
        ""
    );
    // The stored project disagreeing with the engine.
    assert_eq!(
        fatal(AppFailure("project is missing audio strip for input 9".into()).into()),
        "project is missing audio strip for input 9"
    );
}

/// An admitted command whose execution fails must still release its
/// in-flight slot; a leaked slot would eventually refuse every command.
#[test]
fn failed_command_execution_releases_its_inflight_slot() {
    let mut durable = test_project();
    let project_id = durable.project().id();
    let control = Rc::new(RefCell::new(test_control(&durable)));
    let config = ServerConfig::new(
        ServerMode::Development,
        AuthenticationMode::Development,
        std::net::IpAddr::from([127, 0, 0, 1]),
        CAPABILITIES_DIGEST,
    );
    let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
    server.mark_ready().unwrap();
    let principal = development_principal().unwrap();
    let (hello, _) = current_handshake(
        &HandshakeRequest {
            protocol: PROTOCOL_VERSION,
            build: "inflight-leak-test".into(),
            client_type: ClientType::Integration,
            desired_role: Role::Operator,
            resume_cursor: None,
        },
        &control,
        project_id,
    );
    let mut session = server
        .handshake(&hello, &principal, now_millis().unwrap())
        .unwrap()
        .session;
    // A foreign authority makes the post-commit tick of a timed transition
    // fail, so execution returns an error after admission.
    let mut foreign = control_server_identity(&control.borrow(), project_id);
    foreign.log_id = "foreign-log".into();

    let failed = execute_session_command(
        &mut session,
        &control,
        &CountingSaver::default(),
        &mut durable,
        &principal,
        &foreign,
        &test_command(
            "failing-fade",
            "failing-fade-key",
            CommandPayload::Fade { duration_frames: 2 },
        ),
        None,
        None,
    );

    assert!(failed.is_err());
    assert_eq!(session.accounting().inbound_commands_admitted_total, 1);
    assert_eq!(session.accounting().inbound_commands_inflight, 0);
}

/// RC-004: a live client's GO binds Preview and Program on the
/// authority, whatever `bound_preview` and `bound_program` it sends;
/// only its selection is its own. (A journal replay keeps them: it
/// never passes through a session.)
#[test]
fn a_live_fire_binds_preview_and_program_on_the_authority() {
    use fm_protocol::{CueListSpec, CueOperation, WireCue, WireCueAction, WireInputId};
    let mut durable = test_project();
    let project_id = durable.project().id();
    let control = Rc::new(RefCell::new(test_control(&durable)));
    let config = ServerConfig::new(
        ServerMode::Development,
        AuthenticationMode::Development,
        std::net::IpAddr::from([127, 0, 0, 1]),
        CAPABILITIES_DIGEST,
    );
    let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
    server.mark_ready().unwrap();
    let principal = development_principal().unwrap();
    let (hello, _) = current_handshake(
        &HandshakeRequest {
            protocol: PROTOCOL_VERSION,
            build: "live-fire-test".into(),
            client_type: ClientType::Integration,
            desired_role: Role::Admin,
            resume_cursor: None,
        },
        &control,
        project_id,
    );
    let mut session = server
        .handshake(&hello, &principal, now_millis().unwrap())
        .unwrap()
        .session;
    let server_identity = control_server_identity(&control.borrow(), project_id);
    let wire = |value| WireInputId::from_domain(test_input_id(value));
    let spec = CueListSpec {
        name: "Show".into(),
        cues: vec![WireCue {
            id: 10,
            name: "Key Preview".into(),
            auto_follow: false,
            actions: vec![WireCueAction::OverlayOn {
                channel: fm_protocol::WireOverlayChannelId::new(1).unwrap(),
                input: fm_types::InputTarget::Preview,
            }],
        }],
    };
    let commands = [
        test_command(
            "set-cues",
            "set-cues-key",
            CommandPayload::SetCueList {
                list: 7,
                spec: Box::new(spec),
                position: None,
            },
        ),
        // The client claims Program (1) is on Preview and Preview (2)
        // on Program.
        test_command(
            "go",
            "go-key",
            CommandPayload::CueControl {
                list: 7,
                operation: CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings {
                        preview: Some(wire(1)),
                        program: Some(wire(2)),
                        selected: Some(wire(1)),
                    },
                },
            },
        ),
    ];
    for command in &commands {
        let delivery = execute_session_command(
            &mut session,
            &control,
            &CountingSaver::default(),
            &mut durable,
            &principal,
            &server_identity,
            command,
            None,
            None,
        )
        .unwrap();
        assert!(
            matches!(delivery.result, CommandResult::Accepted { .. }),
            "{}: {:?}",
            command.id,
            delivery.result
        );
    }
    let list = durable.project().cue_lists()[0].clone();
    assert_eq!(
        list.state.targets,
        Some(fm_model::TargetBindings {
            preview: Some(test_input_id(2)),
            program: Some(test_input_id(1)),
            selected: Some(test_input_id(1)),
        })
    );
}

#[test]
fn a_refused_manual_position_preserves_restart_state_and_stays_retryable() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "manual-start",
            "manual-start-key",
            CommandPayload::StartManualTransition {
                kind: fm_protocol::ManualTransitionKind::Wipe,
            },
        ),
        0,
    )
    .unwrap();
    let before_durable = durable.clone();
    let before_engine = control.idle_engine_snapshot().unwrap();
    let position_command = test_command(
        "manual-position",
        "manual-position-key",
        CommandPayload::SetManualTransitionPosition {
            position: fm_protocol::ManualTransitionPosition::new(6_250).unwrap(),
        },
    );

    let refused = execute_durable_command(
        &mut control,
        &FailingSaver,
        &mut durable,
        &operator(),
        &server,
        &position_command,
        0,
    )
    .expect("a durability failure is refused, not propagated");

    assert!(matches!(
        &refused.submission.output.result,
        CommandResult::Rejected { code, retryable: true, .. } if code == "unavailable"
    ));
    assert_eq!(durable, before_durable);
    assert_eq!(control.idle_engine_snapshot().unwrap(), before_engine);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &position_command,
        0,
    )
    .unwrap();
    let manual = durable.runtime_manual_transitions();
    assert_eq!(manual.desired.unwrap().position_basis_points, 6_250);
    assert_eq!(manual.realized.unwrap().interval_start_basis_points, 6_250);
    assert_eq!(durable.idempotency_receipts().len(), 2);
}

#[test]
fn save_precedes_commit_ticks_and_returned_result() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let subscription = control.subscribe().unwrap();
    let saver = ObservingSaver {
        subscription: &subscription,
        saved: RefCell::new(None),
    };
    let server = test_server(&control);

    let execution = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "ordered-fade",
            "ordered-fade-key",
            CommandPayload::Fade { duration_frames: 4 },
        ),
        0,
    )
    .unwrap();

    assert!(matches!(
        execution.submission.output.result,
        CommandResult::Accepted { revision: 1, .. }
    ));
    assert_eq!(saver.saved.borrow().as_ref(), Some(&durable));
    assert_eq!(durable.position().frames_rendered, 4);
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Durable(_)
    ));
    assert!(matches!(
        subscription.try_recv().unwrap(),
        LiveEvent::Runtime(_)
    ));
}

/// A cue list whose cue runs a media input's transport restores: the
/// media inputs are known to the show before its cue lists are checked
/// (RC-017).
#[test]
fn a_cue_list_with_a_media_action_restores() {
    let baseline = test_project();
    let mut project = Project::new(
        baseline.project().id(),
        "Cue Media Restore",
        baseline.project().settings().clone(),
    );
    project.add_input(Input {
        id: test_input_id(1),
        name: "Camera".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: test_input_id(2),
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: "asset://clip.mov".into(),
            playback: fm_model::MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    project.set_cue_lists(vec![fm_model::CueList {
        id: 1,
        name: "Show".into(),
        cues: vec![fm_model::Cue {
            id: 1,
            name: "Roll".into(),
            actions: vec![fm_model::CueAction::Media {
                input: fm_types::InputTarget::Fixed(test_input_id(2)),
                operation: fm_model::CueMediaOperation::Play,
            }],
            auto_follow: false,
        }],
        state: fm_model::CueRunState {
            standby: Some(1),
            ..fm_model::CueRunState::default()
        },
    }]);
    let durable = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let engine = restore_engine(&durable).unwrap();
    assert_eq!(engine.show().cue_lists().len(), 1);
}

#[test]
fn projected_project_equals_committed_settled_engine() {
    let mut durable = test_project();
    let original_project = durable.project().clone();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "settled-fade",
            "settled-fade-key",
            CommandPayload::Fade { duration_frames: 7 },
        ),
        0,
    )
    .unwrap();

    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    assert_eq!(durable.position().frames_rendered, 7);
    assert_eq!(durable.position().runtime_generation, 1);
    assert_eq!(durable.position().clock_time_nanos, 240_000_000);
    let mut expected_project = original_project;
    expected_project.set_main_mix(durable.project().main_mix().unwrap());
    assert_eq!(durable.project(), &expected_project);
}

#[test]
fn queued_fade_take_next_settles_before_checkpoint() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let saver = CountingSaver::default();
    let server = test_server(&control);
    let channel = fm_protocol::WireOverlayChannelId::new(1).unwrap();
    let mut execute = |id, key, payload| {
        execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &operator(),
            &server,
            &test_command(id, key, payload),
            0,
        )
    };

    execute(
        "overlay-transition",
        "overlay-transition-key",
        CommandPayload::ConfigureOverlayTransition {
            channel,
            transition: fm_protocol::OverlayTransitionKind::Fade,
            duration_frames: 4,
        },
    )
    .unwrap();
    execute(
        "overlay-queue",
        "overlay-queue-key",
        CommandPayload::QueueOverlay {
            channel,
            source: fm_protocol::WireInputId::from_domain(test_input_id(2)),
        },
    )
    .unwrap();
    let next = execute(
        "overlay-next",
        "overlay-next-key",
        CommandPayload::TakeNextOverlay { channel },
    )
    .unwrap();
    // Dropping the closure ends its captured mutable borrows before the assertions below.
    #[allow(clippy::drop_non_drop)]
    drop(execute);

    assert!(matches!(
        next.submission.output.result,
        CommandResult::Accepted { .. }
    ));
    assert_eq!(durable.position().frames_rendered, 6);
    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    let live = live_engine_snapshot(&mut control);
    assert_eq!(live, restored);
    let channel = OverlayChannelId::new(channel.number()).unwrap();
    let desired = live.show().desired_switcher().overlay(channel);
    let realized = live.realized_switcher().overlay(channel);
    assert_eq!(desired, realized);
    assert_eq!(desired.source(), Some(test_input_id(2)));
    assert!(desired.is_active());
    assert_eq!(desired.opacity(), u8::MAX);
    assert_eq!(desired.queued_sources(), []);
}

#[test]
fn restore_engine_honors_durable_stinger_preload_intent() {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(8).unwrap(),
        test_input_id(2),
        false,
        11,
        ModelStingerAudioPolicy::MixWithProgram,
        StingerMissingMediaFallback::KeepProgram,
    ));
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        true,
        1,
        ModelStingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    ));
    let durable = StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap();

    let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();
    let deferred_slot = StingerSlotId::new(8).unwrap();
    let deferred = StingerDescriptor::new(
        test_input_id(2),
        false,
        11,
        StingerAudioPolicy::MixWithProgram,
        MissingMediaFallback::KeepProgram,
    );
    let preloaded_slot = StingerSlotId::new(1).unwrap();
    let preloaded = StingerDescriptor::new(
        test_input_id(2),
        true,
        1,
        StingerAudioPolicy::Muted,
        MissingMediaFallback::Cut,
    );
    for state in [
        snapshot.show().desired_switcher(),
        snapshot.realized_switcher(),
    ] {
        assert_eq!(state.stinger(deferred_slot).descriptor(), Some(&deferred));
        assert_eq!(
            state.stinger(deferred_slot).preload_state(),
            fm_switcher::StingerPreloadState::NotRequested
        );
        assert_eq!(state.stinger(preloaded_slot).descriptor(), Some(&preloaded));
        assert_eq!(
            state.stinger(preloaded_slot).preload_state(),
            fm_switcher::StingerPreloadState::Ready
        );
    }
}

#[test]
fn missing_cut_stinger_uses_one_checkpoint_frame() {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        false,
        1,
        ModelStingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    ));
    let mut durable = StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    let execution = execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "missing-cut-stinger",
            "missing-cut-stinger-key",
            CommandPayload::Stinger {
                slot: fm_protocol::WireStingerSlotId::new(1).unwrap(),
                duration_frames: 7,
            },
        ),
        0,
    )
    .unwrap();

    assert!(matches!(
        execution.submission.output.result,
        CommandResult::Accepted { .. }
    ));
    assert_eq!(durable.position().frames_rendered, 1);
    assert_eq!(durable.position().runtime_generation, 1);
    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    for state in [
        restored.show().desired_switcher(),
        restored.realized_switcher(),
    ] {
        assert_eq!(
            (state.program(), state.preview()),
            (test_input_id(2), test_input_id(1))
        );
    }
}

#[cfg(feature = "native-media")]
#[test]
fn native_video_ring_partition_preserves_one_aggregate_gpu_limit() {
    let defaults = NativeSourceLimits::default();
    let (ordinary, stingers) = partition_native_source_limits(1_920, 1_080, 1).unwrap();
    let expected_stinger =
        1_920_u64 * 1_080 * 8 * u64::from(defaults.max_video_frames_per_source.get());

    assert_eq!(stingers.max_retained_rgba16f_bytes, expected_stinger);
    assert_eq!(
        ordinary.max_retained_rgba16f_bytes + stingers.max_retained_rgba16f_bytes,
        defaults.max_retained_rgba16f_bytes
    );
    assert_eq!(
        partition_native_source_limits(1_920, 1_080, 5)
            .unwrap_err()
            .to_string(),
        format!(
            "native Stinger rings require {} retained bytes, exceeding aggregate limit {}",
            expected_stinger * 5,
            defaults.max_retained_rgba16f_bytes
        )
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_stinger_retirement_preflight_gate_is_bounded() {
    let retirements = NativeStingerRetirements::start().unwrap();
    let limit = stinger_mutation::retirement_limit_for_test();
    retirements.set_pending_for_test(limit - 1);
    assert!(retirements.can_accept());
    retirements.set_pending_for_test(limit);
    assert!(!retirements.can_accept());
}

#[cfg(feature = "native-media")]
#[test]
fn native_stinger_source_validation_bounds_preload_kinds_and_audio() {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        true,
        1,
        ModelStingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    ));
    let durable = StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        Vec::new(),
    )
    .unwrap();
    let mut source = SimulatedVideoSource::new(
        1,
        1,
        FrameRate::new(25, 1).unwrap(),
        native_clock_domain(),
        SourcePattern::Solid(Rgba8::new(0, 0, 0, 0)),
    )
    .unwrap();
    let retained = NativeResolvedSource::RetainedFrame {
        input: test_input_id(2),
        frame: source.next_frame().unwrap().unwrap(),
    };
    validate_native_stinger_sources(&durable, &[retained]).unwrap();

    let local = NativeResolvedSource::LocalVideo {
        input: test_input_id(2),
        path: PathBuf::from("/secret/stinger.mov"),
    };
    validate_native_stinger_sources(&durable, std::slice::from_ref(&local)).unwrap();
    assert!(stinger_mutation::native_stinger_requires_ffmpeg(
        &durable,
        std::slice::from_ref(&local)
    ));
    assert!(!stinger_mutation::native_stinger_requires_ffmpeg(
        &baseline,
        std::slice::from_ref(&local)
    ));

    let mut audible_project = baseline.project().clone();
    audible_project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        true,
        1,
        ModelStingerAudioPolicy::StingerOnly,
        StingerMissingMediaFallback::Cut,
    ));
    let audible = StoredProject::from_project(
        audible_project,
        baseline.runtime_routing(),
        baseline.position(),
        Vec::new(),
    )
    .unwrap();
    validate_native_stinger_sources(
        &audible,
        &[NativeResolvedSource::LocalVideo {
            input: test_input_id(2),
            path: PathBuf::from("/secret/stinger.mov"),
        }],
    )
    .unwrap();

    let error = validate_native_stinger_sources(
        &durable,
        &[NativeResolvedSource::LiveFrame {
            input: test_input_id(2),
            frame: source.next_frame().unwrap().unwrap(),
        }],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("is live and cannot be preloaded"));

    let mut deferred_project = baseline.project().clone();
    deferred_project.add_stinger(StingerConfig::new(
        StingerSlotNumber::new(1).unwrap(),
        test_input_id(2),
        false,
        1,
        ModelStingerAudioPolicy::Muted,
        StingerMissingMediaFallback::Cut,
    ));
    let deferred = StoredProject::from_project(
        deferred_project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap();
    validate_native_stinger_sources(&deferred, std::slice::from_ref(&local)).unwrap();
    assert!(!stinger_mutation::native_stinger_requires_ffmpeg(
        &deferred,
        &[local]
    ));
}

#[test]
fn alpha_fade_settles_before_checkpoint_and_restores_exact_routing() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "settled-alpha-fade",
            "settled-alpha-fade-key",
            CommandPayload::AlphaFade { duration_frames: 3 },
        ),
        0,
    )
    .unwrap();

    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    assert_eq!(durable.position().frames_rendered, 3);
    assert_eq!(durable.position().runtime_generation, 1);
    assert_eq!(
        (
            restored.realized_switcher().program(),
            restored.realized_switcher().preview(),
        ),
        (test_input_id(2), test_input_id(1))
    );
}

#[test]
fn slide_settles_before_checkpoint_and_restores_exact_routing() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "settled-slide",
            "settled-slide-key",
            CommandPayload::Slide { duration_frames: 3 },
        ),
        0,
    )
    .unwrap();

    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    assert_eq!(durable.position().frames_rendered, 3);
    assert_eq!(durable.position().runtime_generation, 1);
    assert_eq!(
        (
            restored.realized_switcher().program(),
            restored.realized_switcher().preview(),
        ),
        (test_input_id(2), test_input_id(1))
    );
}

#[test]
fn zoom_settles_before_checkpoint_and_restores_exact_routing() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "settled-zoom",
            "settled-zoom-key",
            CommandPayload::Zoom { duration_frames: 3 },
        ),
        0,
    )
    .unwrap();

    let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(live_engine_snapshot(&mut control), restored);
    assert_eq!(durable.position().frames_rendered, 3);
    assert_eq!(durable.position().runtime_generation, 1);
    assert_eq!(
        (
            restored.realized_switcher().program(),
            restored.realized_switcher().preview(),
        ),
        (test_input_id(2), test_input_id(1))
    );
}

#[test]
fn fade_to_black_commands_persist_exact_settled_restart_state() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "fade-to-black",
            "fade-to-black-key",
            CommandPayload::FadeToBlack {
                active: true,
                duration_frames: 4,
            },
        ),
        0,
    )
    .unwrap();

    assert_eq!(
        durable.runtime_fade_to_black(),
        RuntimeFadeToBlack {
            desired: PersistedFadeToBlackState::BLACK,
            realized: PersistedFadeToBlackState::BLACK,
        }
    );
    let black = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert!(black.desired_fade_to_black().active);
    assert_eq!(
        black.desired_fade_to_black().position,
        fm_switcher::FadeToBlackPosition::BLACK
    );
    assert_eq!(live_engine_snapshot(&mut control), black);

    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "fade-from-black",
            "fade-from-black-key",
            CommandPayload::FadeToBlack {
                active: false,
                duration_frames: 3,
            },
        ),
        0,
    )
    .unwrap();

    assert_eq!(
        durable.runtime_fade_to_black(),
        RuntimeFadeToBlack::default()
    );
    let live = restore_engine(&durable).unwrap().snapshot().unwrap();
    assert_eq!(
        live.realized_fade_to_black().position,
        fm_switcher::FadeToBlackPosition::LIVE
    );
    assert_eq!(live_engine_snapshot(&mut control), live);
}

#[test]
fn idle_snapshot_checkpoint_survives_project_store_restart() {
    let root = std::env::temp_dir().join(format!(
        "freemixd-checkpoint-{}-{}.freemix",
        std::process::id(),
        now_millis().unwrap()
    ));
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command("checkpoint-cut", "checkpoint-key", CommandPayload::Cut),
        0,
    )
    .unwrap();
    control.tick(&server).unwrap();

    let checkpoint =
        stored_project_checkpoint(&durable, &control.idle_engine_snapshot().unwrap()).unwrap();
    ProjectStore::new(&root).unwrap().save(&checkpoint).unwrap();
    let restarted = ProjectStore::new(&root).unwrap().load().unwrap();

    assert_eq!(restarted.project(), durable.project());
    assert_eq!(
        restarted.project().input_audio_strip(test_input_id(1)),
        durable.project().input_audio_strip(test_input_id(1))
    );
    assert_eq!(restarted.runtime_routing(), durable.runtime_routing());
    assert_eq!(
        restarted.idempotency_receipts(),
        durable.idempotency_receipts()
    );
    assert_eq!(
        restarted.position().frames_rendered,
        durable.position().frames_rendered + 1
    );
    assert_eq!(restarted.position().revision, durable.position().revision);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn audio_strip_command_checkpoints_every_control() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    execute_durable_command(
        &mut control,
        &CountingSaver::default(),
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "audio-strip",
            "audio-strip-key",
            CommandPayload::SetInputAudioStrip {
                pan_basis_points: 0,
                input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                gain_millidb: -9_000,
                balance_basis_points: 2_500,
                muted: true,
                soloed: true,
                follow_video: false,
                delay_samples: 1_200,
                sends: fm_types::audio_bus::AudioSends::MASTER,
            },
        ),
        0,
    )
    .unwrap();
    control.tick(&server).unwrap();

    let checkpoint =
        stored_project_checkpoint(&durable, &control.idle_engine_snapshot().unwrap()).unwrap();
    assert_eq!(
        checkpoint.project().input_audio_strip(test_input_id(1)),
        Some(InputAudioStripState {
            pan: fm_model::InputPanBasisPoints::CENTER,
            gain: InputGainMilliDb::new(-9_000).unwrap(),
            balance: InputBalanceBasisPoints::new(2_500).unwrap(),
            muted: true,
            soloed: true,
            follow_video: false,
            delay_samples: InputDelaySamples::new(1_200).unwrap(),
            sends: fm_types::audio_bus::AudioSends::MASTER,
        })
    );
}

#[test]
fn authorization_denial_is_saved_before_becoming_replayable() {
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let saver = CountingSaver::default();
    let server = test_server(&control);
    let command = test_command("denied", "denied-key", CommandPayload::Cut);

    let denied = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &viewer(),
        &server,
        &command,
        0,
    )
    .unwrap();

    assert!(matches!(
        denied.submission.output.result,
        CommandResult::Rejected { ref code, .. } if code == "permission_denied"
    ));
    assert_eq!(saver.0.get(), 1);
    assert_eq!(durable.idempotency_receipts().len(), 1);
    let replay = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &viewer(),
        &server,
        &command,
        0,
    )
    .unwrap();
    assert!(replay.submission.replayed);
    assert_eq!(saver.0.get(), 1);
    assert_eq!(replay.runtime_events, []);

    let mut restarted = test_control(&durable);
    let restarted_server = test_server(&restarted);
    let mut restarted_durable = durable.clone();
    let replay_after_restart = execute_durable_command(
        &mut restarted,
        &saver,
        &mut restarted_durable,
        &operator(),
        &restarted_server,
        &command,
        0,
    )
    .unwrap();
    assert!(replay_after_restart.submission.replayed);
    assert_eq!(
        replay_after_restart.submission.output.result,
        denied.submission.output.result
    );
    assert_eq!(saver.0.get(), 1);
    assert_eq!(restarted_durable, durable);
}

#[test]
fn crash_after_save_restarts_from_settled_receipt_without_reexecution() {
    let root = std::env::temp_dir().join(format!(
        "freemixd-{}-{}.freemix",
        std::process::id(),
        now_millis().unwrap()
    ));
    let store = ProjectStore::new(&root).unwrap();
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let command = test_command(
        "crash-fade",
        "crash-fade-key",
        CommandPayload::Fade { duration_frames: 4 },
    );

    let crashed = catch_unwind(AssertUnwindSafe(|| {
        let _ = execute_durable_command(
            &mut control,
            &CrashAfterSave(&store),
            &mut durable,
            &operator(),
            &server,
            &command,
            0,
        );
    }));
    assert!(crashed.is_err());
    assert_eq!(control.diagnostics().current_revision, 0);
    assert_eq!(durable.position().revision, 0);

    let saved = store.load().unwrap();
    assert_eq!(saved.position().revision, 1);
    assert_eq!(saved.position().frames_rendered, 4);
    assert_eq!(saved.idempotency_receipts().len(), 1);
    let mut restarted = test_control(&saved);
    let restarted_server = test_server(&restarted);
    let save_counter = CountingSaver::default();
    let mut restarted_durable = saved.clone();
    let replay = execute_durable_command(
        &mut restarted,
        &save_counter,
        &mut restarted_durable,
        &operator(),
        &restarted_server,
        &command,
        0,
    )
    .unwrap();
    assert!(replay.submission.replayed);
    assert_eq!(replay.runtime_events, []);
    assert_eq!(save_counter.0.get(), 0);
    assert_eq!(restarted_durable, saved);

    fs::remove_dir_all(root).unwrap();
}

#[test]
#[allow(clippy::too_many_lines)]
fn long_show_persists_a_bounded_receipt_window_that_restarts_and_accepts_work() {
    let root = std::env::temp_dir().join(format!(
        "freemixd-receipts-{}-{}.freemix",
        std::process::id(),
        now_millis().unwrap()
    ));
    let store = ProjectStore::new(&root).unwrap();
    let mut durable = test_project();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let commands = MAX_PERSISTED_RECEIPTS + 6;
    let mut project_bytes_at_limit = 0;
    for index in 0..commands {
        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                &format!("cut-{index}"),
                &format!("cut-key-{index:05}"),
                CommandPayload::Cut,
            ),
            0,
        )
        .unwrap();
        assert!(durable.idempotency_receipts().len() <= MAX_PERSISTED_RECEIPTS);
        if index + 1 == MAX_PERSISTED_RECEIPTS {
            store.save(&durable).unwrap();
            project_bytes_at_limit = fs::metadata(root.join("project.json")).unwrap().len();
        }
    }
    assert_eq!(durable.position().revision, commands as u64);
    assert_eq!(durable.idempotency_receipts().len(), MAX_PERSISTED_RECEIPTS);
    let keys = durable
        .idempotency_receipts()
        .iter()
        .map(IdempotencyReceipt::key)
        .collect::<std::collections::BTreeSet<_>>();
    // The oldest receipts were dropped; the newest are all retained.
    for index in 0..6 {
        assert!(!keys.contains(format!("cut-key-{index:05}").as_str()));
    }
    for index in 6..commands {
        assert!(keys.contains(format!("cut-key-{index:05}").as_str()));
    }

    // The bounded project file does not grow past the full window.
    store.save(&durable).unwrap();
    let project_bytes = fs::metadata(root.join("project.json")).unwrap().len();
    assert!(
        project_bytes <= project_bytes_at_limit + 64,
        "{project_bytes} > {project_bytes_at_limit}"
    );

    // A restart restores the window, replays a retained key, and accepts
    // new work with a fresh receipt that keeps the window bounded.
    let saved = store.load().unwrap();
    assert_eq!(saved.idempotency_receipts(), durable.idempotency_receipts());
    let mut restarted = test_control(&saved);
    let restarted_server = test_server(&restarted);
    let mut restarted_durable = saved.clone();
    let newest = format!("cut-key-{:05}", commands - 1);
    let replay = execute_durable_command(
        &mut restarted,
        &CountingSaver::default(),
        &mut restarted_durable,
        &operator(),
        &restarted_server,
        &test_command(
            &format!("cut-{}", commands - 1),
            &newest,
            CommandPayload::Cut,
        ),
        0,
    )
    .unwrap();
    assert!(replay.submission.replayed);
    let fresh = execute_durable_command(
        &mut restarted,
        &CountingSaver::default(),
        &mut restarted_durable,
        &operator(),
        &restarted_server,
        &test_command("after-restart", "after-restart-key", CommandPayload::Cut),
        0,
    )
    .unwrap();
    assert!(!fresh.submission.replayed);
    assert!(matches!(
        fresh.submission.output.result,
        CommandResult::Accepted { revision, .. } if revision == commands as u64 + 1
    ));
    assert_eq!(
        restarted_durable.idempotency_receipts().len(),
        MAX_PERSISTED_RECEIPTS
    );
    assert!(
        restarted_durable
            .idempotency_receipts()
            .iter()
            .any(|receipt| receipt.key() == "after-restart-key")
    );
    store.save(&restarted_durable).unwrap();
    assert_eq!(
        store.load().unwrap().idempotency_receipts().len(),
        MAX_PERSISTED_RECEIPTS
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn receipt_retention_ranks_rejections_after_their_revision_and_keeps_the_pinned_key() {
    let mut receipts = (1..=MAX_PERSISTED_RECEIPTS as u64)
        .map(|revision| {
            IdempotencyReceipt::accepted(format!("a-{revision:05}"), "id", revision, revision)
        })
        .collect::<Vec<_>>();
    // A rejection at revision 1 is newer than the accept that produced 1.
    receipts.push(IdempotencyReceipt::rejected(
        "r-1", "id", 1, "conflict", "stale", false,
    ));
    let retained = retained_receipts(receipts.clone(), None);
    assert_eq!(retained.len(), MAX_PERSISTED_RECEIPTS);
    assert!(!retained.iter().any(|receipt| receipt.key() == "a-00001"));
    assert!(retained.iter().any(|receipt| receipt.key() == "r-1"));

    // An older rejection drops first, unless it is the pinned key.
    receipts.push(IdempotencyReceipt::rejected(
        "r-0", "id", 0, "conflict", "stale", false,
    ));
    let retained = retained_receipts(receipts.clone(), None);
    assert!(!retained.iter().any(|receipt| receipt.key() == "r-0"));
    let retained = retained_receipts(receipts, Some("r-0"));
    assert_eq!(retained.len(), MAX_PERSISTED_RECEIPTS);
    assert!(retained.iter().any(|receipt| receipt.key() == "r-0"));
    // Pinning displaces the next-oldest receipt instead.
    assert!(!retained.iter().any(|receipt| receipt.key() == "r-1"));
    assert!(retained.iter().any(|receipt| receipt.key() == "a-00002"));
}

fn test_project() -> StoredProject {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(42).unwrap()),
        "Unit Test",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1_280, 720).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(44_100).unwrap(),
                sample_format: SampleFormat::I16,
                channels: ChannelLayout::stereo(),
            },
        },
    );
    project.add_input(Input {
        id: test_input_id(1),
        name: "Custom solid and tone".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(7, 11, 13, 255)),
            SimulatedAudio::Sine { frequency_hz: 997 },
        )),
        required_capabilities: vec!["simulation.custom".into()],
    });
    project.add_input(Input {
        id: test_input_id(2),
        name: "Bars".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    assert!(project.set_input_audio_strip(
        test_input_id(1),
        InputAudioStripState {
            pan: fm_model::InputPanBasisPoints::CENTER,
            gain: InputGainMilliDb::new(-3_000).unwrap(),
            balance: InputBalanceBasisPoints::CENTER,
            delay_samples: InputDelaySamples::default(),
            muted: false,
            soloed: false,
            follow_video: true,
            sends: fm_types::audio_bus::AudioSends::MASTER,
        },
    ));
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

fn media_test_project(asset_uri: &str) -> StoredProject {
    let baseline = test_project();
    let mut project = Project::new(
        baseline.project().id(),
        "Media Unit Test",
        baseline.project().settings().clone(),
    );
    for value in [1, 2] {
        project.add_input(Input {
            id: test_input_id(value),
            name: format!("Media {value}"),
            kind: InputKind::Media {
                asset_uri: asset_uri.into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

#[cfg(feature = "native-media")]
fn scene_test_project() -> StoredProject {
    let baseline = test_project();
    let mut project = Project::new(
        baseline.project().id(),
        "Scene Unit Test",
        baseline.project().settings().clone(),
    );
    let scene_id = SceneId::new(NonZeroU128::new(1).unwrap());
    project.add_input(Input {
        id: test_input_id(1),
        name: "Scene".into(),
        kind: InputKind::Scene {
            scene_id,
            audio_source: Some(test_input_id(2)),
        },
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: test_input_id(2),
        name: "Audio".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene_id,
        name: "Program".into(),
        background: ModelRgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
    StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap()
}

fn test_input_id(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

/// Live input add and remove fold into the stored project before they
/// are journaled: an added input persists with the kind its command
/// carried, and a removal the engine allows is still refused, without
/// journaling anything, while a persisted reference the engine does not
/// model (here an alias target) names the input.
#[test]
#[allow(clippy::too_many_lines)]
fn live_input_catalogue_changes_sync_the_project_and_refuse_alias_targets() {
    let baseline = test_project();
    let mut project = baseline.project().clone();
    project.add_input(Input {
        id: test_input_id(3),
        name: "Camera".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: test_input_id(4),
        name: "Camera alias".into(),
        kind: InputKind::Alias {
            target: test_input_id(3),
            crop: None,
            effects: Vec::new(),
        },
        required_capabilities: Vec::new(),
    });
    let mut durable = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(test_input_id(1)),
            realized_program_id: Some(test_input_id(1)),
            desired_preview_id: Some(test_input_id(2)),
            realized_preview_id: Some(test_input_id(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    let mut control = test_control(&durable);
    let server = test_server(&control);
    let saver = CountingSaver::default();
    let before = durable.clone();

    let refused = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "remove-target",
            "remove-target-key",
            CommandPayload::RemoveInput {
                input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
            },
        ),
        0,
    )
    .unwrap();
    assert!(matches!(
        &refused.submission.output.result,
        CommandResult::Rejected {
            code,
            message,
            retryable: false,
            current_revision: 0,
            ..
        } if code == "conflict"
            && message.contains("cannot be removed: it is the target of alias input")
    ));
    // The persisted reference travels as a typed reason (Protocol 2.45).
    let CommandResult::Rejected { reason, .. } = &refused.submission.output.result else {
        unreachable!()
    };
    assert!(
        matches!(
            reason,
            Some(fm_protocol::RefusalReason::InputInUse {
                input: 3,
                usage: fm_protocol::InputUse::AliasTarget { .. },
            })
        ),
        "{reason:?}"
    );
    assert_eq!(saver.0.get(), 0, "a refused removal journals nothing");
    assert_eq!(durable, before);

    let added = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "add-guest",
            "add-guest-key",
            CommandPayload::AddInput {
                input: fm_protocol::WireInputId::from_domain(test_input_id(5)),
                name: "Guest".into(),
                source: fm_protocol::LiveInputSource::Solid {
                    red: 1,
                    green: 2,
                    blue: 3,
                    alpha: 255,
                },
            },
        ),
        0,
    )
    .unwrap();
    assert!(matches!(
        added.submission.output.result,
        CommandResult::Accepted { revision: 1, .. }
    ));
    let guest = durable
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == test_input_id(5))
        .expect("the added input is persisted");
    assert_eq!(guest.name, "Guest");
    assert_eq!(
        guest.kind,
        InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(1, 2, 3, 255)),
            SimulatedAudio::Silence,
        ))
    );
    assert!(
        durable
            .project()
            .input_audio_strips()
            .iter()
            .any(|strip| strip.input == test_input_id(5))
    );

    let effects = vec![fm_protocol::WireLayerEffect {
        name: "saturation".into(),
        parameters: vec![-50],
    }];
    let edited = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "alias-effects",
            "alias-effects-key",
            CommandPayload::SetAliasEffects {
                input: fm_protocol::WireInputId::from_domain(test_input_id(4)),
                effects: effects.clone(),
            },
        ),
        0,
    )
    .unwrap();
    assert!(matches!(
        edited.submission.output.result,
        CommandResult::Accepted { revision: 2, .. }
    ));
    let alias = durable
        .project()
        .inputs()
        .iter()
        .find(|input| input.id == test_input_id(4))
        .expect("the alias stays persisted");
    assert_eq!(
        alias.kind,
        InputKind::Alias {
            target: test_input_id(3),
            crop: None,
            effects: vec![fm_model::LayerEffect::new("saturation", vec![-50])],
        }
    );

    let removed = execute_durable_command(
        &mut control,
        &saver,
        &mut durable,
        &operator(),
        &server,
        &test_command(
            "remove-alias",
            "remove-alias-key",
            CommandPayload::RemoveInput {
                input: fm_protocol::WireInputId::from_domain(test_input_id(4)),
            },
        ),
        0,
    )
    .unwrap();
    assert!(matches!(
        removed.submission.output.result,
        CommandResult::Accepted { revision: 3, .. }
    ));
    assert_eq!(
        durable
            .project()
            .inputs()
            .iter()
            .map(|input| input.id)
            .collect::<Vec<_>>(),
        [1, 2, 3, 5].map(test_input_id)
    );
    assert_eq!(saver.0.get(), 3);
}

#[cfg(feature = "native-media")]
#[test]
fn native_stinger_preflight_rejections_are_path_free_and_retryable_by_resubmission() {
    let slot = fm_protocol::WireStingerSlotId::new(8).unwrap();
    for payload in [
        CommandPayload::ConfigureStinger {
            slot,
            media_input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
            preload: true,
            cut_point_frames: 3,
            audio_policy: fm_protocol::StingerAudioPolicy::Muted,
            missing_media_fallback: fm_protocol::StingerMissingMediaFallback::Cut,
        },
        CommandPayload::RemoveStinger { slot },
    ] {
        let result =
            native_stinger_preflight_rejection(&test_command("native", "native-key", payload), 7);
        assert!(matches!(
            result,
            CommandResult::Rejected {
                ref code,
                ref message,
                current_revision: 7,
                retryable: false,
                ..
            } if code == "unavailable"
                && message == "native Stinger resources could not be prepared"
        ));
    }
}

fn test_control(project: &StoredProject) -> ControlService<Policy> {
    ControlService::new(
        restore_engine(project).unwrap(),
        Policy::development(),
        "unit-engine",
        "unit-log",
        ControlLimits::default(),
    )
}

fn test_server(control: &ControlService<Policy>) -> ServerIdentity {
    let engine = control.diagnostics().engine;
    ServerIdentity {
        engine_id: engine.engine_id,
        project_id: "42".into(),
        state_epoch: engine.state_epoch,
        log_id: engine.log_id,
    }
}

fn test_command(id: &str, key: &str, payload: CommandPayload) -> CommandMessage {
    CommandMessage {
        protocol: PROTOCOL_VERSION,
        id: id.into(),
        idempotency_key: key.into(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    }
}

fn operator() -> Principal {
    development_principal().unwrap()
}

fn viewer() -> Principal {
    Principal::authenticated(
        UserId::new("unit-viewer").unwrap(),
        SessionId::new("unit-session").unwrap(),
        [AuthRole::Viewer],
    )
}

fn live_engine_snapshot(control: &mut ControlService<Policy>) -> EngineSnapshot {
    let prepared = control
        .prepare_submit(
            &viewer(),
            test_command("snapshot-probe", "snapshot-probe-key", CommandPayload::Cut),
            0,
        )
        .unwrap()
        .prepared()
        .unwrap();
    let snapshot = prepared.project(0).unwrap();
    prepared.abort();
    snapshot
}

const STREAM_TARGET_A: u128 = 7_001;
const STREAM_TARGET_B: u128 = 7_002;

/// A project with one routed output and two stream destinations, the first
/// carrying an authored desired-running intent when `running[0]` is true.
fn test_project_with_streams(running: [bool; 2]) -> StoredProject {
    use fm_model::{
        AudioBus, Output, Rgba8 as TestRgba8, Scene as TestScene, StartupPolicy as ModelStartup,
        StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId,
    };
    use fm_types::{BusId as TestBusId, OutputId as TestOutputId, SceneId as TestSceneId};

    let baseline = test_project();
    let mut project = baseline.project().clone();
    let scene_id = TestSceneId::new(NonZeroU128::new(900).unwrap());
    let bus_id = TestBusId::new(NonZeroU128::new(901).unwrap());
    let output_id = TestOutputId::new(NonZeroU128::new(902).unwrap());
    project.add_scene(TestScene {
        id: scene_id,
        name: "Program".into(),
        background: TestRgba8::OPAQUE_BLACK,
        layers: Vec::new(),
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_audio_bus(AudioBus {
        id: bus_id,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: output_id,
        name: "Program output".into(),
        video_source: scene_id,
        audio_source: bus_id,
        startup: ModelStartup::ReconcileDesiredState,
        required_capabilities: Vec::new(),
    });
    for (index, number) in [STREAM_TARGET_A, STREAM_TARGET_B].into_iter().enumerate() {
        let target = StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(number).unwrap()),
            format!(
                "Destination {}",
                u128::from(u8::try_from(index).unwrap() + 1)
            ),
            StreamProtocol::Rtmp,
            StreamEndpoint::parse("ingest.example.com/live").unwrap(),
            Some(StreamKey::parse("unit-secret-key").unwrap()),
            output_id,
        )
        .unwrap()
        .with_startup(ModelStartup::ReconcileDesiredState)
        .set_running(running[index]);
        project.add_stream_target_checked(target).unwrap();
    }
    StoredProject::from_project(
        project,
        baseline.runtime_routing(),
        baseline.position(),
        baseline.idempotency_receipts().to_vec(),
    )
    .unwrap()
}
