#[test]
fn parses_serve_options() {
    assert_eq!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--listen",
            "127.0.0.1:9123",
            "--once",
        ]))
        .unwrap(),
        Command::Serve {
            project: "show.freemix".into(),
            listen: "127.0.0.1:9123".parse().unwrap(),
            web_listen: None,
            web: web::WebSettings::default(),
            status_listen: None,
            hls_listen: None,
            live_lan_listen: None,
            vmix_listen: None,
            vmix_http_listen: None,
            once: true,
            native_media: false,
            fullscreen_program: false,
            display_outputs: Vec::new(),
            display_inventory: None,
            presenter_fallback: false,
            camera_helper: None,
            capture_inventory: None,
            record_program: None,
            record_output: None,
            record_program_2: None,
            record_output_2: None,
            record_wav: false,
            record_segment_seconds: [None; RECORDER_SLOTS],
            record_bitrate_kbps: None,
            record_bitrate_kbps_2: None,
            record_audio_bitrate_kbps: None,
            record_audio_bitrate_kbps_2: None,
            record_fallback_dir: None,
            diagnostic_stop_after: None,
            recover_to_checkpoint: false,
        }
    );
    let web = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--web-listen",
        "127.0.0.1:9234",
    ]))
    .unwrap();
    assert!(matches!(
        web,
        Command::Serve {
            web_listen: Some(address),
            once: false,
            native_media: false,
            ..
        } if address == "127.0.0.1:9234".parse::<SocketAddr>().unwrap()
    ));
    assert!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--web-listen",
            "127.0.0.1:9234",
            "--once",
        ]))
        .is_err()
    );
    // The native daemon serves the gateway too.
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--web-listen",
            "127.0.0.1:9234",
            "--native-media",
        ]))
        .unwrap(),
        Command::Serve {
            web_listen: Some(_),
            native_media: true,
            ..
        }
    ));
    // The windowed program daemon serves the gateway too.
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--web-listen",
            "127.0.0.1:9234",
            "--native-media",
            "--fullscreen-program",
        ]))
        .unwrap(),
        Command::Serve {
            web_listen: Some(_),
            fullscreen_program: true,
            ..
        }
    ));
}

#[test]
fn default_serve_is_ephemeral_loopback() {
    let command = parse_args(strings(&["serve", "show.freemix"])).unwrap();
    assert!(matches!(
        command,
        Command::Serve { listen, once: false, native_media: false, .. }
            if listen == DEFAULT_LISTEN.parse::<SocketAddr>().unwrap()
    ));
}

/// OR-017: `--live-lan-listen` takes any address (a LAN one included)
/// but needs native media.
#[test]
fn parses_live_lan_listen_as_a_native_option() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--live-lan-listen",
            "0.0.0.0:8090",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--live-lan-listen=0.0.0.0:8090",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                live_lan_listen: Some(address),
                native_media: true,
                ..
            } if address == "0.0.0.0:8090".parse().unwrap()
        ));
    }
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--live-lan-listen", "0.0.0.0:8090"]),
            "--live-lan-listen requires --native-media",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--live-lan-listen=0.0.0.0:1",
                "--live-lan-listen",
                "0.0.0.0:2",
            ]),
            "duplicate option `--live-lan-listen`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--live-lan-listen",
            ]),
            "missing value for --live-lan-listen",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

/// RC-009: `--vmix-listen` is loopback-only and needs no native media.
#[test]
fn parses_vmix_listen_only_as_a_loopback_option() {
    for arguments in [
        strings(&["serve", "show.freemix", "--vmix-listen", "127.0.0.1:0"]),
        strings(&["serve", "show.freemix", "--vmix-listen=127.0.0.1:0"]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                vmix_listen: Some(address),
                ..
            } if address == "127.0.0.1:0".parse().unwrap()
        ));
    }
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--vmix-listen", "0.0.0.0:8090"]),
            "--vmix-listen must use a loopback address",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--vmix-listen=127.0.0.1:1",
                "--vmix-listen",
                "127.0.0.1:2",
            ]),
            "duplicate option `--vmix-listen`",
        ),
        (
            strings(&["serve", "show.freemix", "--vmix-listen"]),
            "missing value for --vmix-listen",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--vmix-listen",
                "127.0.0.1:0",
            ]),
            "--vmix-listen cannot be combined with --fullscreen-program",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_vmix_http_listen_only_as_a_loopback_option() {
    for arguments in [
        strings(&["serve", "show.freemix", "--vmix-http-listen", "127.0.0.1:0"]),
        strings(&["serve", "show.freemix", "--vmix-http-listen=127.0.0.1:0"]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                vmix_http_listen: Some(address),
                ..
            } if address == "127.0.0.1:0".parse().unwrap()
        ));
    }
    for (arguments, expected) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--vmix-http-listen",
                "0.0.0.0:8088",
            ]),
            "--vmix-http-listen must use a loopback address",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--vmix-http-listen=127.0.0.1:1",
                "--vmix-http-listen",
                "127.0.0.1:2",
            ]),
            "duplicate option `--vmix-http-listen`",
        ),
        (
            strings(&["serve", "show.freemix", "--vmix-http-listen"]),
            "missing value for --vmix-http-listen",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--vmix-http-listen",
                "127.0.0.1:0",
            ]),
            "--vmix-http-listen cannot be combined with --fullscreen-program",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_hls_listen_only_as_a_loopback_native_option() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--hls-listen",
            "127.0.0.1:0",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--hls-listen=127.0.0.1:9432",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                hls_listen: Some(address),
                native_media: true,
                ..
            } if address.ip().is_loopback()
        ));
    }
    for (arguments, expected) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen",
                "0.0.0.0:9000",
            ]),
            "--hls-listen must use a loopback address",
        ),
        (
            strings(&["serve", "show.freemix", "--hls-listen", "127.0.0.1:9000"]),
            "--hls-listen requires --native-media",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--hls-listen",
                "127.0.0.1:9000",
            ]),
            "--hls-listen cannot be combined with --fullscreen-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen",
                "127.0.0.1:9001",
                "--hls-listen",
                "127.0.0.1:9002",
            ]),
            "duplicate option `--hls-listen`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen=127.0.0.1:9001",
                "--hls-listen=127.0.0.1:9002",
            ]),
            "duplicate option `--hls-listen`",
        ),
        (
            strings(&["serve", "show.freemix", "--native-media", "--hls-listen"]),
            "missing value for --hls-listen",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
    // Like `--status-listen`, the `=` form parses its value directly, so an
    // empty value is an invalid address rather than a missing argument.
    assert!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--hls-listen=",
        ]))
        .unwrap_err()
        .to_string()
        .contains("invalid hls listen address")
    );
}

#[test]
fn hls_token_bounds_helper_requires_bounded_ascii_graphic_bytes() {
    let short = [b'a'; hls_server::HLS_TOKEN_MIN_BYTES - 1];
    assert!(!hls_server::valid_token(&short));
    let minimum = [b'a'; hls_server::HLS_TOKEN_MIN_BYTES];
    assert!(hls_server::valid_token(&minimum));
    let maximum = [b'a'; hls_server::HLS_TOKEN_MAX_BYTES];
    assert!(hls_server::valid_token(&maximum));
    let long = [b'a'; hls_server::HLS_TOKEN_MAX_BYTES + 1];
    assert!(!hls_server::valid_token(&long));
    for byte in [b' ', b'\n', b'\r', 0x7f] {
        let mut non_graphic = minimum;
        non_graphic[0] = byte;
        assert!(!hls_server::valid_token(&non_graphic), "{byte:#04x}");
    }
}

#[cfg(feature = "native-media")]
#[test]
fn hls_roots_helper_skips_non_hls_destinations_and_keys_hls_targets() {
    let roots = hls_roots_from_destinations([
        (OutputProtocol::Rtmp, 1, PathBuf::from("/srv/rtmp")),
        (OutputProtocol::Hls, 3, PathBuf::from("/srv/hls-a")),
        (OutputProtocol::Srt, 2, PathBuf::from("/srv/srt")),
        (OutputProtocol::Hls, 4, PathBuf::from("/srv/hls-b")),
    ])
    .unwrap();
    assert_eq!(
        roots.resolve(3, "index.m3u8"),
        Ok(PathBuf::from("/srv/hls-a/index.m3u8"))
    );
    assert_eq!(
        roots.resolve(4, "7/segment-00042.ts"),
        Ok(PathBuf::from("/srv/hls-b/7/segment-00042.ts"))
    );
    assert_eq!(
        roots.resolve(1, "index.m3u8"),
        Err(hls_server::HlsRouteError::NotFound)
    );
    assert_eq!(
        roots.resolve(2, "index.m3u8"),
        Err(hls_server::HlsRouteError::NotFound)
    );
    assert!(matches!(
        hls_roots_from_destinations([
            (OutputProtocol::Hls, 5, PathBuf::from("/srv/one")),
            (OutputProtocol::Hls, 5, PathBuf::from("/srv/two")),
        ]),
        Err(hls_server::HlsRouteError::DuplicateTarget)
    ));
}

#[test]
fn parses_native_media_as_an_opt_in() {
    assert!(matches!(
        parse_args(strings(&["serve", "show.freemix", "--native-media"])).unwrap(),
        Command::Serve {
            native_media: true,
            ..
        }
    ));
}

#[test]
fn rejects_duplicate_listen_option() {
    let error = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--listen",
        "127.0.0.1:4100",
        "--listen",
        "127.0.0.1:4200",
    ]))
    .unwrap_err();
    assert_eq!(error.to_string(), "duplicate option `--listen`");
}

#[test]
fn rejects_duplicate_native_media_option() {
    let error = parse_args(strings(&[
        "serve",
        "show.freemix",
        "--native-media",
        "--native-media",
    ]))
    .unwrap_err();
    assert_eq!(error.to_string(), "duplicate option `--native-media`");
}

#[test]
fn parses_camera_helper_only_with_native_media() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--camera-helper",
            "helper-bin",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--camera-helper=helper-bin",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                camera_helper: Some(path),
                ..
            } if path == Path::new("helper-bin")
        ));
    }
    assert_eq!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--camera-helper",
            "helper-bin",
        ]))
        .unwrap_err()
        .to_string(),
        "--camera-helper requires --native-media"
    );
    for arguments in [
        strings(&["serve", "show.freemix", "--native-media", "--camera-helper"]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--camera-helper=",
        ]),
    ] {
        assert_eq!(
            parse_args(arguments).unwrap_err().to_string(),
            "missing value for --camera-helper"
        );
    }
    assert_eq!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--camera-helper=a",
            "--camera-helper",
            "b",
        ]))
        .unwrap_err()
        .to_string(),
        "duplicate option `--camera-helper`"
    );
}

#[test]
fn parses_bounded_diagnostic_stop_duration() {
    assert_eq!(
        parse_diagnostic_duration("1500ms").unwrap(),
        Duration::from_millis(1_500)
    );
    assert_eq!(
        parse_diagnostic_duration("2s").unwrap(),
        Duration::from_secs(2)
    );
    assert_eq!(
        parse_diagnostic_duration("3m").unwrap(),
        Duration::from_mins(3)
    );
    assert_eq!(
        parse_diagnostic_duration("24h").unwrap(),
        Duration::from_hours(24)
    );
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--diagnostic-stop-after=10m",
        ]))
        .unwrap(),
        Command::Serve {
            diagnostic_stop_after: Some(duration),
            ..
        } if duration == Duration::from_mins(10)
    ));
}

#[test]
fn rejects_invalid_diagnostic_stop_options() {
    for (arguments, expected) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--once",
                "--diagnostic-stop-after",
                "1s",
            ]),
            "--diagnostic-stop-after cannot be combined with --once",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--diagnostic-stop-after",
                "1s",
            ]),
            "--diagnostic-stop-after currently supports headless native mode only",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--diagnostic-stop-after",
                "0s",
            ]),
            "diagnostic duration must be nonzero",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--diagnostic-stop-after",
                "25h",
            ]),
            "diagnostic duration cannot exceed 24h",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--diagnostic-stop-after",
                "1s",
                "--diagnostic-stop-after=2s",
            ]),
            "duplicate option `--diagnostic-stop-after`",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_program_recording_with_once_and_fullscreen() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--record-program",
            "capture.mp4",
            "--once",
        ]))
        .unwrap(),
        Command::Serve {
            once: true,
            native_media: true,
            fullscreen_program: true,
            record_program: Some(path),
            ..
        } if path == Path::new("capture.mp4")
    ));
}

#[test]
fn parses_equals_program_recording_with_option_like_file_name() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program=--capture.mp4",
        ]))
        .unwrap(),
        Command::Serve {
            record_program: Some(path),
            ..
        } if path == Path::new("--capture.mp4")
    ));
}

#[test]
fn rejects_invalid_program_recording_option_relationships() {
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--record-program", "capture.mp4"]),
            "--record-program requires --native-media",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
            ]),
            "missing value for --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "--once",
            ]),
            "missing value for --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "one.mp4",
                "--record-program",
                "two.mp4",
            ]),
            "duplicate option `--record-program`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program=",
            ]),
            "missing value for --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program=one.mp4",
                "--record-program=two.mp4",
            ]),
            "duplicate option `--record-program`",
        ),
        (
            strings(&["serve", "show.freemix", "--native-media", "--record-wav"]),
            "--record-wav requires --record-program or --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-wav",
                "--record-wav",
            ]),
            "duplicate option `--record-wav`",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn rejects_invalid_second_program_recording_options() {
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--record-program-2", "capture.mp4"]),
            "--record-program-2 requires --native-media",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
            ]),
            "missing value for --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "--once",
            ]),
            "missing value for --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "one.mp4",
                "--record-program-2",
                "two.mp4",
            ]),
            "duplicate option `--record-program-2`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2=",
            ]),
            "missing value for --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2=one.mp4",
                "--record-program-2=two.mp4",
            ]),
            "duplicate option `--record-program-2`",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_second_program_recording_slot() {
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2",
            "backup.mp4",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2=backup.mp4",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                record_program: None,
                record_program_2: Some(path),
                ..
            } if path == Path::new("backup.mp4")
        ));
    }
    // An option-like file name stays reachable through the `=` form.
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2=--backup.mp4",
        ]))
        .unwrap(),
        Command::Serve {
            record_program_2: Some(path),
            ..
        } if path == Path::new("--backup.mp4")
    ));
    // Both slots configure side by side from one desired flag.
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
            "--record-program-2",
            "backup.mp4",
        ]))
        .unwrap(),
        Command::Serve {
            record_program: Some(primary),
            record_program_2: Some(secondary),
            ..
        } if primary == Path::new("capture.mp4") && secondary == Path::new("backup.mp4")
    ));
}

#[test]
fn parses_wav_sidecar_for_the_second_recorder_slot() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2",
            "backup.mp4",
            "--record-wav",
        ]))
        .unwrap(),
        Command::Serve {
            record_program: None,
            record_program_2: Some(path),
            record_wav: true,
            ..
        } if path == Path::new("backup.mp4")
    ));
}

#[test]
fn parses_program_wav_sidecar_recording() {
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
            "--record-wav",
        ]))
        .unwrap(),
        Command::Serve {
            record_program: Some(path),
            record_wav: true,
            ..
        } if path == Path::new("capture.mp4")
    ));
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
        ]))
        .unwrap(),
        Command::Serve {
            record_wav: false,
            ..
        }
    ));
}

#[test]
fn parses_record_output_for_both_recorder_slots() {
    let output_a = fm_types::OutputId::new(NonZeroU128::new(10).unwrap());
    let output_b = fm_types::OutputId::new(NonZeroU128::new(20).unwrap());
    for arguments in [
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
            "--record-output",
            "10",
            "--record-program-2",
            "backup.mp4",
            "--record-output-2",
            "20",
        ]),
        strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program=capture.mp4",
            "--record-output=10",
            "--record-program-2=backup.mp4",
            "--record-output-2=20",
        ]),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                record_program: Some(primary),
                record_output: Some(primary_output),
                record_program_2: Some(secondary),
                record_output_2: Some(secondary_output),
                ..
            } if primary == Path::new("capture.mp4")
                && primary_output == output_a
                && secondary == Path::new("backup.mp4")
                && secondary_output == output_b
        ));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn rejects_invalid_record_output_options_and_values() {
    for (arguments, expected) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-output",
                "1",
            ]),
            "--record-output requires --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output-2",
                "2",
            ]),
            "--record-output-2 requires --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output",
                "1",
            ]),
            "--record-output requires --record-program",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
            ]),
            "missing value for --record-output",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
                "--once",
            ]),
            "missing value for --record-output",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output=",
            ]),
            "missing value for --record-output",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
            ]),
            "missing value for --record-output-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
                "--once",
            ]),
            "missing value for --record-output-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2=",
            ]),
            "missing value for --record-output-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
                "1",
                "--record-output",
                "2",
            ]),
            "duplicate option `--record-output`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output=1",
                "--record-output=2",
            ]),
            "duplicate option `--record-output`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
                "1",
                "--record-output-2",
                "2",
            ]),
            "duplicate option `--record-output-2`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2=1",
                "--record-output-2=2",
            ]),
            "duplicate option `--record-output-2`",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
                "0",
            ]),
            "invalid value for --record-output `0`; expected a positive integer output id",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output=abc",
            ]),
            "invalid value for --record-output `abc`; expected a positive integer output id",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
                "0",
            ]),
            "invalid value for --record-output-2 `0`; expected a positive integer output id",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2=-5",
            ]),
            "invalid value for --record-output-2 `-5`; expected a positive integer output id",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn parses_record_segment_seconds_per_slot_with_either_form_and_inclusive_bounds() {
    let seconds = |value| NonZeroU32::new(value).unwrap();
    for (arguments, expected) in [
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
                "1",
            ]),
            [Some(seconds(1)), None],
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=86400",
            ]),
            [Some(seconds(86_400)), None],
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
                "1",
            ]),
            [None, Some(seconds(1))],
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=86400",
            ]),
            [None, Some(seconds(86_400))],
        ),
        (
            record_args(&[
                "--record-program=capture.mp4",
                "--record-program-2=backup.mp4",
                "--record-segment-seconds",
                "300",
                "--record-segment-seconds-2=600",
            ]),
            [Some(seconds(300)), Some(seconds(600))],
        ),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                record_segment_seconds,
                ..
            } if record_segment_seconds == expected
        ));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn rejects_invalid_record_segment_seconds_options_and_values() {
    for (arguments, expected) in [
        (
            record_args(&["--record-segment-seconds", "60"]),
            "--record-segment-seconds requires --record-program",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds",
                "60",
            ]),
            "--record-segment-seconds requires --record-program",
        ),
        (
            record_args(&["--record-segment-seconds-2", "60"]),
            "--record-segment-seconds-2 requires --record-program-2",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds-2",
                "60",
            ]),
            "--record-segment-seconds-2 requires --record-program-2",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
            ]),
            "missing value for --record-segment-seconds",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
                "--once",
            ]),
            "missing value for --record-segment-seconds",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=",
            ]),
            "missing value for --record-segment-seconds",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
            ]),
            "missing value for --record-segment-seconds-2",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
                "--once",
            ]),
            "missing value for --record-segment-seconds-2",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=",
            ]),
            "missing value for --record-segment-seconds-2",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
                "10",
                "--record-segment-seconds",
                "20",
            ]),
            "duplicate option `--record-segment-seconds`",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=10",
                "--record-segment-seconds=20",
            ]),
            "duplicate option `--record-segment-seconds`",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
                "10",
                "--record-segment-seconds-2",
                "20",
            ]),
            "duplicate option `--record-segment-seconds-2`",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=10",
                "--record-segment-seconds-2=20",
            ]),
            "duplicate option `--record-segment-seconds-2`",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds",
                "0",
            ]),
            "invalid value for --record-segment-seconds `0`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=86401",
            ]),
            "invalid value for --record-segment-seconds `86401`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=abc",
            ]),
            "invalid value for --record-segment-seconds `abc`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2",
                "0",
            ]),
            "invalid value for --record-segment-seconds-2 `0`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=86401",
            ]),
            "invalid value for --record-segment-seconds-2 `86401`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=abc",
            ]),
            "invalid value for --record-segment-seconds-2 `abc`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-segment-seconds=-5",
            ]),
            "invalid value for --record-segment-seconds `-5`; expected an integer in 1..=86400",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-segment-seconds-2=-5",
            ]),
            "invalid value for --record-segment-seconds-2 `-5`; expected an integer in 1..=86400",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[cfg(feature = "native-media")]
#[test]
fn slot_segment_seconds_selects_each_slots_own_rotation_duration() {
    let first_only: [Option<NonZeroU32>; RECORDER_SLOTS] =
        [Some(NonZeroU32::new(10).unwrap()), None];
    assert_eq!(
        slot_segment_seconds(first_only, 0).map(NonZeroU32::get),
        Some(10)
    );
    assert_eq!(slot_segment_seconds(first_only, 1), None);

    let second_only: [Option<NonZeroU32>; RECORDER_SLOTS] =
        [None, Some(NonZeroU32::new(86_400).unwrap())];
    assert_eq!(slot_segment_seconds(second_only, 0), None);
    assert_eq!(
        slot_segment_seconds(second_only, 1).map(NonZeroU32::get),
        Some(86_400)
    );
}

#[test]
fn parses_record_bitrate_options_with_either_form_and_inclusive_bounds() {
    for (arguments, video, video_2, audio, audio_2) in [
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps",
                "2000",
                "--record-audio-bitrate-kbps",
                "96",
            ]),
            Some(2_000),
            None,
            Some(96),
            None,
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2=3000",
                "--record-audio-bitrate-kbps-2=128",
            ]),
            None,
            Some(3_000),
            None,
            Some(128),
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program=capture.mp4",
                "--record-program-2=backup.mp4",
                "--record-bitrate-kbps=1000",
                "--record-bitrate-kbps-2",
                "100000",
                "--record-audio-bitrate-kbps",
                "8",
                "--record-audio-bitrate-kbps-2=1024",
            ]),
            Some(MIN_RECORD_VIDEO_BITRATE_KBPS),
            Some(MAX_RECORD_VIDEO_BITRATE_KBPS),
            Some(MIN_RECORD_AUDIO_BITRATE_KBPS),
            Some(MAX_RECORD_AUDIO_BITRATE_KBPS),
        ),
    ] {
        assert!(matches!(
            parse_args(arguments).unwrap(),
            Command::Serve {
                record_bitrate_kbps,
                record_bitrate_kbps_2,
                record_audio_bitrate_kbps,
                record_audio_bitrate_kbps_2,
                ..
            } if record_bitrate_kbps == video
                && record_bitrate_kbps_2 == video_2
                && record_audio_bitrate_kbps == audio
                && record_audio_bitrate_kbps_2 == audio_2
        ));
    }
}

#[test]
fn parses_and_validates_the_record_fallback_directory() {
    for flag in [
        &["--record-fallback-dir", "spare"][..],
        &["--record-fallback-dir=spare"][..],
    ] {
        let mut arguments = vec![
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program",
            "capture.mp4",
        ];
        arguments.extend_from_slice(flag);
        assert!(matches!(
            parse_args(strings(&arguments)).unwrap(),
            Command::Serve {
                record_fallback_dir: Some(directory),
                ..
            } if directory == Path::new("spare")
        ));
    }
    assert!(matches!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--record-program-2",
            "backup.mp4",
            "--record-fallback-dir",
            "spare",
        ]))
        .unwrap(),
        Command::Serve {
            record_fallback_dir: Some(_),
            ..
        }
    ));
    for (arguments, expected) in [
        (
            strings(&["serve", "show.freemix", "--record-fallback-dir", "spare"]),
            "--record-fallback-dir requires --record-program or --record-program-2",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-fallback-dir",
            ]),
            "missing value for --record-fallback-dir",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-fallback-dir=",
            ]),
            "missing value for --record-fallback-dir",
        ),
        (
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-fallback-dir",
                "a",
                "--record-fallback-dir=b",
            ]),
            "duplicate option `--record-fallback-dir`",
        ),
    ] {
        let error = parse_args(arguments).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
    assert!(
        parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--fullscreen-program",
            "--record-program",
            "capture.mp4",
            "--record-fallback-dir",
            "spare",
        ]))
        .is_ok(),
        "a fullscreen program now accepts a fallback directory"
    );
}

#[cfg(feature = "native-media")]
#[test]
fn recording_sessions_refuse_invalid_stems_and_a_fallback_in_the_output_directory() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("capture.mp4");
    validate_recording_sessions([("--record-program", Some(output.as_path()))], None).unwrap();
    let spaced = directory.path().join("my show.mp4");
    let error = validate_recording_sessions([("--record-program-2", Some(spaced.as_path()))], None)
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("--record-program-2 output stem must be"),
        "{error}"
    );
    let error = validate_recording_sessions(
        [("--record-program", Some(output.as_path()))],
        Some(directory.path()),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        error,
        "--record-fallback-dir must differ from the --record-program output directory"
    );
    assert!(!error.contains(&*directory.path().to_string_lossy()));
    validate_recording_sessions(
        [("--record-program", Some(output.as_path()))],
        Some(&directory.path().join("spare")),
    )
    .unwrap();
}

#[test]
fn rejects_record_bitrate_options_without_their_recorder_slot_or_value() {
    for (arguments, expected) in [
        (
            record_args(&["--record-bitrate-kbps", "2000"]),
            "--record-bitrate-kbps requires --record-program",
        ),
        (
            record_args(&["--record-bitrate-kbps-2=2000"]),
            "--record-bitrate-kbps-2 requires --record-program-2",
        ),
        (
            record_args(&["--record-audio-bitrate-kbps", "96"]),
            "--record-audio-bitrate-kbps requires --record-program",
        ),
        (
            record_args(&["--record-audio-bitrate-kbps-2=96"]),
            "--record-audio-bitrate-kbps-2 requires --record-program-2",
        ),
        (
            record_args(&["--record-program", "capture.mp4", "--record-bitrate-kbps"]),
            "missing value for --record-bitrate-kbps",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps",
                "--once",
            ]),
            "missing value for --record-bitrate-kbps",
        ),
        (
            record_args(&["--record-program", "capture.mp4", "--record-bitrate-kbps="]),
            "missing value for --record-bitrate-kbps",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2",
            ]),
            "missing value for --record-bitrate-kbps-2",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2=",
            ]),
            "missing value for --record-bitrate-kbps-2",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps",
            ]),
            "missing value for --record-audio-bitrate-kbps",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps=",
            ]),
            "missing value for --record-audio-bitrate-kbps",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-audio-bitrate-kbps-2",
            ]),
            "missing value for --record-audio-bitrate-kbps-2",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-audio-bitrate-kbps-2=",
            ]),
            "missing value for --record-audio-bitrate-kbps-2",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn rejects_out_of_range_record_bitrate_options() {
    for (arguments, expected) in [
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps",
                "999",
            ]),
            "invalid value for --record-bitrate-kbps `999`; expected an integer in 1000..=100000",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps=100001",
            ]),
            "invalid value for --record-bitrate-kbps `100001`; expected an integer in 1000..=100000",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps=abc",
            ]),
            "invalid value for --record-bitrate-kbps `abc`; expected an integer in 1000..=100000",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2",
                "100001",
            ]),
            "invalid value for --record-bitrate-kbps-2 `100001`; expected an integer in 1000..=100000",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps",
                "7",
            ]),
            "invalid value for --record-audio-bitrate-kbps `7`; expected an integer in 8..=1024",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps=1025",
            ]),
            "invalid value for --record-audio-bitrate-kbps `1025`; expected an integer in 8..=1024",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-audio-bitrate-kbps-2=-5",
            ]),
            "invalid value for --record-audio-bitrate-kbps-2 `-5`; expected an integer in 8..=1024",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[test]
fn rejects_duplicate_record_bitrate_options() {
    for (arguments, expected) in [
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps",
                "2000",
                "--record-bitrate-kbps",
                "3000",
            ]),
            "duplicate option `--record-bitrate-kbps`",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-bitrate-kbps=2000",
                "--record-bitrate-kbps=3000",
            ]),
            "duplicate option `--record-bitrate-kbps`",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-bitrate-kbps-2",
                "2000",
                "--record-bitrate-kbps-2=3000",
            ]),
            "duplicate option `--record-bitrate-kbps-2`",
        ),
        (
            record_args(&[
                "--record-program",
                "capture.mp4",
                "--record-audio-bitrate-kbps",
                "96",
                "--record-audio-bitrate-kbps=128",
            ]),
            "duplicate option `--record-audio-bitrate-kbps`",
        ),
        (
            record_args(&[
                "--record-program-2",
                "backup.mp4",
                "--record-audio-bitrate-kbps-2=96",
                "--record-audio-bitrate-kbps-2=128",
            ]),
            "duplicate option `--record-audio-bitrate-kbps-2`",
        ),
    ] {
        assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
    }
}

#[cfg(feature = "native-media")]
#[test]
fn segment_frame_limit_rounds_up_to_cover_requested_seconds_at_integer_and_fractional_rates() {
    let sec = |value| NonZeroU32::new(value).unwrap();
    let rate_25 = FrameRate::new(25, 1).unwrap();
    let rate_30 = FrameRate::new(30, 1).unwrap();
    let rate_60 = FrameRate::new(60, 1).unwrap();
    let rate_23_976 = FrameRate::new(24_000, 1_001).unwrap();
    let rate_29_97 = FrameRate::new(30_000, 1_001).unwrap();
    let rate_59_94 = FrameRate::new(60_000, 1_001).unwrap();

    assert_eq!(segment_frame_limit(sec(1), rate_25), 25);
    assert_eq!(segment_frame_limit(sec(2), rate_30), 60);
    assert_eq!(segment_frame_limit(sec(10), rate_60), 600);
    assert_eq!(segment_frame_limit(sec(86_400), rate_60), 86_400 * 60);

    // Fractional NTSC rates round up so the segment spans at least the
    // requested wall duration and hits an exact frame boundary at 1001 s.
    assert_eq!(segment_frame_limit(sec(1), rate_23_976), 24);
    assert_eq!(segment_frame_limit(sec(1), rate_29_97), 30);
    assert_eq!(segment_frame_limit(sec(1), rate_59_94), 60);
    assert_eq!(segment_frame_limit(sec(1_001), rate_23_976), 24_000);
    assert_eq!(segment_frame_limit(sec(1_001), rate_29_97), 30_000);
    assert_eq!(segment_frame_limit(sec(1_001), rate_59_94), 60_000);
}

#[test]
fn recording_output_selection_validation_accepts_known_and_rejects_unknown_outputs() {
    let project = test_project_with_streams([false, false]).project().clone();
    let known_output = fm_types::OutputId::new(NonZeroU128::new(902).unwrap());
    let unknown_output = fm_types::OutputId::new(NonZeroU128::new(999).unwrap());

    validate_recording_output_selections(&project, None, None).unwrap();
    validate_recording_output_selections(&project, Some(known_output), Some(known_output)).unwrap();
    assert_eq!(
        validate_recording_output_selections(&project, Some(unknown_output), None)
            .unwrap_err()
            .to_string(),
        "--record-output selects unknown project output 999"
    );
    assert_eq!(
        validate_recording_output_selections(&project, Some(known_output), Some(unknown_output))
            .unwrap_err()
            .to_string(),
        "--record-output-2 selects unknown project output 999"
    );
}

#[test]
fn resolve_project_output_entry_selects_distinct_outputs_and_defaults_to_first() {
    fn resolve<'a>(
        selected: Option<fm_types::OutputId>,
        frames: &'a [(fm_types::OutputId, &'static str)],
        unbound: Option<&'a str>,
    ) -> Option<&'a str> {
        resolve_project_output_entry(
            selected,
            frames,
            unbound,
            |(id, _)| *id,
            |(_, label)| *label,
        )
    }

    let out_1 = fm_types::OutputId::new(NonZeroU128::new(1).unwrap());
    let out_2 = fm_types::OutputId::new(NonZeroU128::new(2).unwrap());
    let out_missing = fm_types::OutputId::new(NonZeroU128::new(3).unwrap());
    let outputs = [(out_1, "program-main"), (out_2, "program-clean")];

    assert_eq!(
        resolve(None, &outputs, Some("unbound")),
        Some("program-main")
    );
    assert_eq!(
        resolve(Some(out_1), &outputs, Some("unbound")),
        Some("program-main")
    );
    assert_eq!(
        resolve(Some(out_2), &outputs, Some("unbound")),
        Some("program-clean")
    );
    assert_eq!(resolve(Some(out_missing), &outputs, Some("unbound")), None);
    assert_eq!(resolve(None, &[], Some("unbound")), Some("unbound"));
    assert_eq!(resolve(Some(out_1), &[], Some("unbound")), Some("unbound"));
    assert_eq!(resolve(None, &[], None), None);
}

#[test]
fn capability_digest_distinguishes_recorder_modes() {
    assert_eq!(
        capabilities_digest(false, false, false),
        CAPABILITIES_DIGEST
    );
    assert_eq!(
        capabilities_digest(true, false, false),
        NATIVE_MEDIA_CAPABILITIES_DIGEST
    );
    assert_eq!(
        capabilities_digest(true, false, true),
        PROGRAM_RECORDER_CAPABILITIES_DIGEST
    );
    assert_eq!(
        capabilities_digest(true, true, false),
        FULLSCREEN_PROGRAM_CAPABILITIES_DIGEST
    );
    assert_eq!(
        capabilities_digest(true, true, true),
        FULLSCREEN_PROGRAM_RECORDER_CAPABILITIES_DIGEST
    );
}

#[cfg(feature = "native-media")]
#[test]
fn native_telemetry_diagnostic_names_scoped_metrics_and_unavailable_gpu_time() {
    let origin = Instant::now();
    let adapter = fm_gpu::NativeAdapterInfo {
        name: "Test Adapter".to_owned(),
        backend: NativeBackend::Metal,
    };
    let mut telemetry = NativeRuntimeTelemetry::new(origin, &adapter);
    telemetry.audio_retained_bytes = 10;
    telemetry.audio_peak_retained_bytes = 11;
    telemetry.audio_reservation_requests = 12;
    telemetry.audio_reserved_blocks = 13;
    telemetry.audio_peak_reserved_blocks = 14;
    telemetry.audio_reserved_samples = 15;
    telemetry.audio_peak_reserved_samples = 16;
    telemetry.audio_reserved_bytes = 17;
    telemetry.audio_peak_reserved_bytes = 18;
    telemetry.audio_source_stalls = 19;
    telemetry.audio_positioned_blocks = 20;
    telemetry.audio_positioned_samples = 21;
    telemetry.audio_leading_silence_samples = 22;
    telemetry.audio_eos_padding_blocks = 23;
    telemetry.audio_eos_padding_samples = 24;
    let deadline = origin
        .checked_sub(Duration::from_millis(10))
        .expect("test deadline is representable");
    telemetry.observe_host_lateness(deadline, origin);
    let diagnostic = telemetry.diagnostic(Some(fm_gpu::PresentationTelemetry {
        pending_depth: 1,
        peak_pending_depth: 1,
        frames_dropped: 2,
        ..fm_gpu::PresentationTelemetry::default()
    }));

    assert!(diagnostic.starts_with("FREEMIXD_TELEMETRY\tv=4\t"));
    assert!(diagnostic.contains("\thost_lateness_samples_total=1\t"));
    assert!(diagnostic.contains("\thost_lateness_samples_retained=1\t"));
    assert!(diagnostic.contains("\thost_lateness_p50_ms=10.000\t"));
    assert!(diagnostic.contains("\taudio_retained_bytes=10\t"));
    assert!(diagnostic.contains("\taudio_observed_peak_retained_bytes=11\t"));
    assert!(diagnostic.contains("\taudio_reservation_requests=12\t"));
    assert!(diagnostic.contains("\taudio_reserved_blocks=13\t"));
    assert!(diagnostic.contains("\taudio_observed_peak_reserved_blocks=14\t"));
    assert!(diagnostic.contains("\taudio_reserved_samples=15\t"));
    assert!(diagnostic.contains("\taudio_observed_peak_reserved_samples=16\t"));
    assert!(diagnostic.contains("\taudio_reserved_bytes=17\t"));
    assert!(diagnostic.contains("\taudio_observed_peak_reserved_bytes=18\t"));
    assert!(diagnostic.contains("\taudio_source_stalls=19\t"));
    assert!(diagnostic.contains("\taudio_positioned_blocks=20\t"));
    assert!(diagnostic.contains("\taudio_positioned_samples=21\t"));
    assert!(diagnostic.contains("\taudio_leading_silence_samples=22\t"));
    assert!(diagnostic.contains("\taudio_eos_padding_blocks=23\t"));
    assert!(diagnostic.contains("\taudio_eos_padding_samples=24\t"));
    assert!(diagnostic.contains("\tpresentation_active=true\t"));
    assert!(diagnostic.contains("\tpresentation_pending_depth=1\t"));
    assert!(diagnostic.contains("\tpresentation_dropped=2\t"));
    assert!(diagnostic.contains("\trecorder_configured=false\t"));
    assert!(diagnostic.contains("\tcamera_configured_sources=0\t"));
    assert!(diagnostic.contains("\tcamera_frames_received=0\t"));
    assert!(diagnostic.contains("\tcamera_frames_ingested=0\t"));
    assert!(diagnostic.contains("\tgpu_backend=Metal\t"));
    assert!(diagnostic.contains("\tgpu_adapter=Test Adapter\t"));
    assert!(diagnostic.contains("\tgpu_timing=Unsupported\t"));
    assert!(diagnostic.contains("\tgpu_pass_samples_total=0\t"));
    assert!(diagnostic.contains("\tgpu_pass_samples_retained=0\t"));
    assert!(diagnostic.contains("\tgpu_pass_p50_ms=none\t"));
    assert!(diagnostic.contains("\tmetric_errors=0\t"));
    assert!(diagnostic.contains("\tdisk_alert=unknown\t"));
    assert!(diagnostic.ends_with("\tdisk_free_bytes=0"));
}

#[cfg(feature = "native-media")]
#[test]
fn disk_alert_label_maps_alert_states() {
    assert_eq!(disk_alert_label(None), "unknown");
    assert_eq!(disk_alert_label(Some(AlertState::Inactive)), "normal");
    assert_eq!(disk_alert_label(Some(AlertState::Firing)), "low");
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn camera_source_test_telemetry() -> NativeCameraSourceTelemetry {
    NativeCameraSourceTelemetry {
        input: test_input_id(1),
        lifecycle: LifecycleState::Running,
        health: EndpointHealthState::Healthy,
        frames_received: 20,
        frames_ingested: 6,
        native_dropped: 2,
        queue_dropped: 1,
        queue_depth: 1,
        queue_peak_depth: 2,
        continuity_rejected: 0,
        recovery_timeout_discarded: 0,
        terminal_error_discarded: 1,
        terminal_trigger_discarded: 1,
        ready_delivery_depth: 1,
        ready_delivery_discarded: 1,
        cancellation_discarded: 1,
        supervisor_slot_replaced: 3,
        supervisor_slot_depth: 1,
        ingest_failed: 1,
        preflight_depth: 1,
        preflight_discarded: 1,
        recovery_attempts: 2,
        recovery_successes: 1,
        recovery_exhausted: 0,
        recovery_worker_failures: 0,
        recovery_outcome: CameraRecoveryOutcome::Recovered,
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_source_diagnostics_are_stable_bounded_and_saturating() {
    assert_eq!(lifecycle_label(LifecycleState::Closed), "closed");
    assert_eq!(lifecycle_label(LifecycleState::Open), "open");
    assert_eq!(lifecycle_label(LifecycleState::Running), "running");
    assert_eq!(lifecycle_label(LifecycleState::Lost), "lost");
    assert_eq!(lifecycle_label(LifecycleState::Recovering), "recovering");
    assert_eq!(health_label(EndpointHealthState::Healthy), "healthy");
    assert_eq!(health_label(EndpointHealthState::Degraded), "degraded");
    assert_eq!(health_label(EndpointHealthState::SignalLost), "signal_lost");
    assert_eq!(health_label(EndpointHealthState::Failed), "failed");

    let source = camera_source_test_telemetry();
    let diagnostic = source.diagnostic();
    assert!(
        diagnostic.starts_with(
            "FREEMIXD_CAMERA_SOURCE\tv=1\tclassification=diagnostic-not-certification\t"
        )
    );
    assert!(diagnostic.contains("\tsample_phase=pre_cleanup\t"));
    assert!(diagnostic.contains("\tsample_lifecycle=running\thealth=healthy\t"));
    assert!(diagnostic.contains("\trecovery_attempts=2\t"));
    assert!(diagnostic.contains("\tsupervisor_slot_replaced=3\t"));
    assert!(diagnostic.contains("\tsupervisor_slot_depth=1\t"));
    assert!(diagnostic.contains("\tterminal_error_discarded=1\t"));
    assert!(diagnostic.contains("\tterminal_trigger_discarded=1\t"));
    assert!(diagnostic.contains("\tready_delivery_depth=1\t"));
    assert!(diagnostic.contains("\tready_delivery_discarded=1\t"));
    assert!(diagnostic.contains("\tcancellation_discarded=1\t"));
    assert!(diagnostic.contains("\tingest_failed=1\t"));
    assert!(diagnostic.contains("\tpreflight_depth=1\t"));
    assert!(diagnostic.contains("\tpreflight_discarded=1\t"));
    assert!(diagnostic.contains("\trecovery_outcome=recovered"));
    assert!(diagnostic.len() < 768);
    assert_eq!(source.frames_received, source.accounted_frames());

    let mut second = source;
    second.input = test_input_id(2);
    let mut unsorted = [second, source];
    sort_camera_source_telemetry(&mut unsorted);
    assert_eq!(
        unsorted.map(|sample| sample.input),
        [test_input_id(1), test_input_id(2)]
    );

    let mut saturated = source;
    saturated.frames_received = u64::MAX;
    saturated.frames_ingested = u64::MAX;
    saturated.native_dropped = u64::MAX;
    saturated.queue_dropped = u64::MAX;
    saturated.queue_depth = u64::MAX;
    saturated.queue_peak_depth = u64::MAX;
    saturated.continuity_rejected = u64::MAX;
    saturated.recovery_timeout_discarded = u64::MAX;
    saturated.terminal_error_discarded = u64::MAX;
    saturated.terminal_trigger_discarded = u64::MAX;
    saturated.ready_delivery_depth = u64::MAX;
    saturated.ready_delivery_discarded = u64::MAX;
    saturated.cancellation_discarded = u64::MAX;
    saturated.supervisor_slot_replaced = u64::MAX;
    saturated.supervisor_slot_depth = u64::MAX;
    saturated.ingest_failed = u64::MAX;
    saturated.preflight_depth = u64::MAX;
    saturated.preflight_discarded = u64::MAX;
    saturated.recovery_attempts = u64::MAX;
    saturated.recovery_successes = u64::MAX;
    saturated.recovery_exhausted = u64::MAX;
    saturated.recovery_worker_failures = u64::MAX;
    let aggregate = aggregate_camera_telemetry(&[saturated, source]);
    assert_eq!(aggregate.configured_sources, 2);
    assert_eq!(aggregate.frames_received, u64::MAX);
    assert_eq!(aggregate.frames_ingested, u64::MAX);
    assert_eq!(aggregate.native_dropped, u64::MAX);
    assert_eq!(aggregate.queue_dropped, u64::MAX);
    assert_eq!(aggregate.queue_depth, u64::MAX);
    assert_eq!(aggregate.queue_peak_depth, u64::MAX);
    assert_eq!(aggregate.continuity_rejected, u64::MAX);
    assert_eq!(aggregate.recovery_timeout_discarded, u64::MAX);
    assert_eq!(aggregate.terminal_error_discarded, u64::MAX);
    assert_eq!(aggregate.terminal_trigger_discarded, u64::MAX);
    assert_eq!(aggregate.ready_delivery_depth, u64::MAX);
    assert_eq!(aggregate.ready_delivery_discarded, u64::MAX);
    assert_eq!(aggregate.cancellation_discarded, u64::MAX);
    assert_eq!(aggregate.supervisor_slot_replaced, u64::MAX);
    assert_eq!(aggregate.supervisor_slot_depth, u64::MAX);
    assert_eq!(aggregate.ingest_failed, u64::MAX);
    assert_eq!(aggregate.preflight_depth, u64::MAX);
    assert_eq!(aggregate.preflight_discarded, u64::MAX);
    assert_eq!(aggregate.recovery_attempts, u64::MAX);
    assert_eq!(aggregate.recovery_successes, u64::MAX);
    assert_eq!(aggregate.recovery_exhausted, u64::MAX);
    assert_eq!(aggregate.recovery_worker_failures, u64::MAX);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_latest_slot_carries_discontinuity_across_replacement_race() {
    let mut slot = CameraFrameSlot::default();
    slot.replace(camera_slot_test_frame(8, true));
    slot.replace(camera_slot_test_frame(9, false));
    slot.replace(camera_slot_test_frame(10, false));

    let newest = slot.take().unwrap();
    assert_eq!(newest.timing().sequence().get(), 10);
    assert!(
        newest
            .timing()
            .flags()
            .contains(fm_frame::MediaFlags::DISCONTINUITY)
    );
    assert_eq!(slot.replacements, 2);
    assert!(slot.take().is_none());
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[test]
fn camera_ingest_failure_is_an_exact_accounted_drop() {
    let mut frame = CameraFrameSlot::default();
    frame.replace(camera_slot_test_frame(7, false));
    let supervisor = Arc::new(Mutex::new(CameraSupervisorState {
        frame,
        snapshot: CameraWorkerSnapshot {
            telemetry: CameraTelemetry {
                received: 1,
                ..CameraTelemetry::default()
            },
            lifecycle: LifecycleState::Running,
            health: EndpointHealthState::Healthy,
            recovery_attempts: 0,
            recovery_successes: 0,
            recovery_exhausted: 0,
            recovery_worker_failures: 0,
            recovery_outcome: CameraRecoveryOutcome::Never,
        },
    }));
    let mut cameras = NativeCameraInputs {
        inputs: vec![NativeCameraInput {
            input: test_input_id(1),
            source: None,
            worker: None,
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

    let error = cameras
        .poll_with(|_, _| Err(AppFailure("injected ingest failure".to_owned()).into()))
        .unwrap_err();
    assert_eq!(error.to_string(), "injected ingest failure");
    let telemetry = cameras.source_telemetry()[0];
    assert_eq!(telemetry.frames_received, 1);
    assert_eq!(telemetry.frames_ingested, 0);
    assert_eq!(telemetry.ingest_failed, 1);
    assert_eq!(telemetry.queue_depth, 0);
    assert_eq!(telemetry.supervisor_slot_depth, 0);
    assert_eq!(telemetry.frames_received, telemetry.accounted_frames());
}

#[cfg(feature = "native-media")]
#[test]
fn recorder_capability_is_configured_support_not_live_health() {
    let digest = capabilities_digest(true, false, true);
    let mut policy = RecorderCapturePolicy::default();
    assert!(policy.fail("backend:failed".to_owned(), false).is_some());
    assert_eq!(digest, capabilities_digest(true, false, true));
    assert!(!policy.active());
}

#[cfg(feature = "native-media")]
#[test]
fn startup_pair_barrier_requires_mux_output_and_prioritizes_failure() {
    assert_eq!(
        startup_pair_decision(RecorderState::Recording, 0, 0, false),
        StartupPairDecision::Pending
    );
    assert_eq!(
        startup_pair_decision(RecorderState::Recording, 1, 0, false),
        StartupPairDecision::Pending
    );
    assert_eq!(
        startup_pair_decision(RecorderState::Recording, 1, 1, false),
        StartupPairDecision::Ready
    );
    assert_eq!(
        startup_pair_decision(RecorderState::Recording, 1, 1, true),
        StartupPairDecision::Failed
    );
    assert_eq!(
        startup_pair_decision(RecorderState::Failed, 1, 1, true),
        StartupPairDecision::Failed
    );
}

#[cfg(feature = "native-media")]
#[test]
fn next_record_segment_path_prefers_the_original_and_skips_existing_names() {
    let directory = tempfile::tempdir().unwrap();
    let base = |name: &str| directory.path().join(name);
    let exists = |path: &Path| path.exists();

    // Segment 1 is the configured path itself.
    assert_eq!(
        next_record_segment_path(&base("show.mp4"), exists).unwrap(),
        base("show.mp4")
    );
    std::fs::write(base("show.mp4"), b"").unwrap();
    // Later segments are zero-padded `<stem>-<NNN>.mp4`, first free wins.
    assert_eq!(
        next_record_segment_path(&base("show.mp4"), exists).unwrap(),
        base("show-002.mp4")
    );
    std::fs::write(base("show-002.mp4"), b"").unwrap();
    assert_eq!(
        next_record_segment_path(&base("show.mp4"), exists).unwrap(),
        base("show-003.mp4")
    );
    // Gaps are filled by the first free name, not the last.
    std::fs::create_dir_all(directory.path().join("nested")).unwrap();
    let nested = |name: &str| directory.path().join("nested").join(name);
    for name in ["take.mp4", "take-002.mp4", "take-004.mp4"] {
        std::fs::write(nested(name), b"").unwrap();
    }
    assert_eq!(
        next_record_segment_path(&nested("take.mp4"), exists).unwrap(),
        nested("take-003.mp4")
    );
    // Compound stems keep everything before the final extension.
    std::fs::write(base("a.b.mp4"), b"").unwrap();
    assert_eq!(
        next_record_segment_path(&base("a.b.mp4"), exists).unwrap(),
        base("a.b-002.mp4")
    );
    // Non-.mp4 outputs and unnamed paths are refused outright.
    for refused in ["show.mkv", "show", ".mp4"] {
        assert!(
            next_record_segment_path(&base(refused), exists).is_err(),
            "{refused} must be refused"
        );
    }
}

#[cfg(feature = "native-media")]
#[test]
fn next_record_segment_path_refuses_exhaustion() {
    const LAST: u32 = RECORD_SEGMENT_LAST_ORDINAL;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("show.mp4");
    std::fs::write(&base, b"").unwrap();
    for ordinal in 2..=LAST {
        std::fs::write(directory.path().join(format!("show-{ordinal:03}.mp4")), b"").unwrap();
    }
    let error = next_record_segment_path(&base, Path::exists).unwrap_err();
    assert!(error.to_string().contains("exhausted"));
}

#[cfg(feature = "native-media")]
#[test]
fn second_recorder_slot_derives_segments_from_its_own_configured_path() {
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("program.mp4");
    let secondary = directory.path().join("backup.mp4");
    std::fs::write(&primary, b"").unwrap();
    std::fs::write(&secondary, b"").unwrap();
    let derive = |slot| {
        next_slot_segment_path(slot, Some(&primary), Some(&secondary), Path::exists).unwrap()
    };
    assert_eq!(derive(0).unwrap(), directory.path().join("program-002.mp4"));
    // Slot 1 derives from `--record-program-2` alone, so two recorders
    // writing into one directory fill their own gaps.
    assert_eq!(derive(1).unwrap(), directory.path().join("backup-002.mp4"));
    // An unconfigured slot derives nothing instead of borrowing the
    // other slot's base path.
    assert_eq!(
        next_slot_segment_path(1, Some(&primary), None, Path::exists).unwrap(),
        None
    );
    assert_eq!(
        next_slot_segment_path(0, None, Some(&secondary), Path::exists).unwrap(),
        None
    );
}

#[test]
fn recording_parent_falls_back_to_the_working_directory() {
    assert_eq!(recording_parent(Path::new("capture.mp4")), Path::new("."));
    assert_eq!(
        recording_parent(Path::new("out/capture.mp4")),
        Path::new("out")
    );
}

#[cfg(feature = "native-media")]
#[test]
fn record_wav_sidecar_shares_each_segment_stem() {
    let directory = tempfile::tempdir().unwrap();
    let names = recording_session::SegmentNames::new(None);
    for (segment, sidecar) in [
        ("program.mp4", "program.wav"),
        ("program-002.mp4", "program-002.wav"),
    ] {
        let planned = recording_session::plan(
            &directory.path().join(segment),
            directory.path(),
            &names,
            1,
            1,
            Some(1),
        )
        .unwrap();
        assert_eq!(
            planned.wav_config().unwrap().segment_path(0, 1).unwrap(),
            directory.path().join(sidecar)
        );
    }
}

#[cfg(feature = "native-media")]
#[test]
fn recorded_duration_is_exact_for_integer_and_fractional_rates() {
    let thirty = fm_types::FrameRate::new(30, 1).unwrap();
    assert_eq!(recorded_duration_ms(0, thirty), 0);
    assert_eq!(recorded_duration_ms(90, thirty), 3_000);
    let drop_frame = fm_types::FrameRate::new(30_000, 1_001).unwrap();
    assert_eq!(recorded_duration_ms(30, drop_frame), 1_001);
    assert_eq!(recorded_duration_ms(60, drop_frame), 2_002);
    let sixty = fm_types::FrameRate::new(60, 1).unwrap();
    assert_eq!(recorded_duration_ms(1, sixty), 16);
    assert_eq!(recorded_duration_ms(u64::MAX, thirty), u64::MAX);
}

#[cfg(feature = "native-media")]
#[test]
fn recording_start_timecode_derives_components_at_integer_and_fractional_rates() {
    let components = |sequence: u64, rate: fm_types::FrameRate| {
        recording_start_timecode(sequence, rate).map(fm_types::Timecode::components)
    };
    let thirty = fm_types::FrameRate::new(30, 1).unwrap();
    assert_eq!(components(0, thirty), Some((0, 0, 0, 0)));
    assert_eq!(components(29, thirty), Some((0, 0, 0, 29)));
    assert_eq!(components(30, thirty), Some((0, 0, 1, 0)));
    assert_eq!(components(30 * 3_600, thirty), Some((1, 0, 0, 0)));
    // The hour component wraps modulo 24 at exactly 24 hours and at 25
    // hours, whose minute, second, and frame components restart at zero.
    assert_eq!(components(30 * 3_600 * 24, thirty), Some((0, 0, 0, 0)));
    assert_eq!(components(30 * 3_600 * 25, thirty), Some((1, 0, 0, 0)));

    // Fractional NTSC rates derive the rounded nominal integer cadence.
    let rate_29_97 = fm_types::FrameRate::new(30_000, 1_001).unwrap();
    let rate_23_976 = fm_types::FrameRate::new(24_000, 1_001).unwrap();
    let rate_59_94 = fm_types::FrameRate::new(60_000, 1_001).unwrap();
    assert_eq!(components(29, rate_29_97), Some((0, 0, 0, 29)));
    assert_eq!(components(30, rate_29_97), Some((0, 0, 1, 0)));
    assert_eq!(components(23, rate_23_976), Some((0, 0, 0, 23)));
    assert_eq!(components(24, rate_23_976), Some((0, 0, 1, 0)));
    assert_eq!(components(59, rate_59_94), Some((0, 0, 0, 59)));
    assert_eq!(components(60, rate_59_94), Some((0, 0, 1, 0)));
    assert_eq!(components(30 * 3_600, rate_29_97), Some((1, 0, 0, 0)));
}

#[cfg(feature = "native-media")]
#[test]
fn stream_encoder_settings_carry_the_authored_video_bitrate() {
    use fm_model::{StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId};

    let target = StreamTarget::new(
        StreamTargetId::new(NonZeroU128::new(7_100).unwrap()),
        "Bitrate".to_owned(),
        StreamProtocol::Rtmp,
        StreamEndpoint::parse("ingest.example.com/live").unwrap(),
        Some(StreamKey::parse("unit-secret-key").unwrap()),
        fm_types::OutputId::new(NonZeroU128::new(7_101).unwrap()),
    )
    .unwrap();
    let authored = target.clone().with_video_bitrate(9_000).unwrap();
    assert_ne!(authored.video_bitrate_kbps(), target.video_bitrate_kbps());
    assert_eq!(
        stream_encoder_settings(authored.video_bitrate_kbps()).video_bitrate_kbps,
        9_000
    );
    // The model default maps onto the sink default unchanged.
    let settings = stream_encoder_settings(target.video_bitrate_kbps());
    assert_eq!(
        settings.video_bitrate_kbps,
        EncoderSettings::default().video_bitrate_kbps
    );
    assert_eq!(
        settings.audio_bitrate_kbps,
        EncoderSettings::default().audio_bitrate_kbps
    );
}

#[cfg(feature = "native-media")]
#[test]
fn stream_ladder_maps_authored_rungs_and_ignores_keyed_destinations() {
    use fm_model::{
        StreamEndpoint, StreamKey, StreamLadder, StreamLadderRung, StreamProtocol, StreamTarget,
        StreamTargetId,
    };

    let output = fm_types::OutputId::new(NonZeroU128::new(7_111).unwrap());
    let (protocol, endpoint) = StreamEndpoint::parse_url("hls:///srv/freemix/live").unwrap();
    let ladder = StreamLadder::new(vec![
        StreamLadderRung::new(1_000, 640, 360).unwrap(),
        StreamLadderRung::new(2_500, 1_280, 720).unwrap(),
    ])
    .unwrap();
    let laddered = StreamTarget::new(
        StreamTargetId::new(NonZeroU128::new(7_110).unwrap()),
        "Ladder".to_owned(),
        protocol,
        endpoint,
        None,
        output,
    )
    .unwrap()
    .with_ladder(Some(ladder))
    .unwrap();
    let mapped = stream_ladder(&laddered).unwrap();
    assert_eq!(mapped.rungs().len(), 2);
    assert_eq!(mapped.top_bitrate_kbps(), 2_500);
    assert_eq!(mapped.rungs()[0].video_bitrate_kbps, 1_000);
    assert_eq!(mapped.rungs()[0].width, 640);
    assert_eq!(mapped.rungs()[0].height, 360);
    assert_eq!(mapped.rungs()[1].video_bitrate_kbps, 2_500);
    assert_eq!(mapped.rungs()[1].width, 1_280);
    assert_eq!(mapped.rungs()[1].height, 720);
    // The base bitrate a laddered target advertises is its top rung.
    assert_eq!(laddered.video_bitrate_kbps(), 2_500);

    let keyed = StreamTarget::new(
        StreamTargetId::new(NonZeroU128::new(7_112).unwrap()),
        "Plain".to_owned(),
        StreamProtocol::Rtmp,
        StreamEndpoint::parse("ingest.example.com/live").unwrap(),
        Some(StreamKey::parse("unit-secret-key").unwrap()),
        output,
    )
    .unwrap();
    assert!(stream_ladder(&keyed).is_none());
}

/// Work that is not durable is never acknowledged.
///
/// The journal is the only thing standing between an accepted command and
/// its acknowledgement, so while it cannot record, every command must come
/// back refused and retryable with no revision consumed, the daemon must
/// stay up and keep serving, and once recording works again the next
/// command takes the very next revision. The fault stands in for a failing
/// or full show disk at the exact seam the daemon hands its journal; file
/// surgery under a running daemon cannot inject it, because the journal
/// database is opened once, at startup, and held for the daemon's run.
#[test]
#[allow(clippy::too_many_lines)]
fn unavailable_journal_refuses_commands_and_the_session_keeps_serving() {
    struct SwitchableJournal {
        available: Arc<AtomicBool>,
        recorded: AtomicU64,
    }

    impl DurableStore for SwitchableJournal {
        fn record(
            &self,
            _command: &CommandMessage,
            _now_millis: u64,
            _began_frame: u64,
            _previous: &StoredProject,
            _updated: &StoredProject,
        ) -> AppResult<()> {
            if !self.available.load(Ordering::SeqCst) {
                return Err(AppFailure("injected journal unavailability".into()).into());
            }
            self.recorded.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
            if !self.available.load(Ordering::SeqCst) {
                return Err(AppFailure("injected journal unavailability".into()).into());
            }
            Ok(())
        }
    }

    fn next_command_result(reader: &mut MessageReader) -> CommandResult {
        loop {
            match reader.read_message_with_idle(|| Ok(false)).unwrap() {
                Some(WireMessage::CommandResult(result)) => return result,
                Some(_) => {}
                None => panic!("expected a command result"),
            }
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let available = Arc::new(AtomicBool::new(true));
    let journal_available = Arc::clone(&available);
    let (served_tx, served_rx) = std::sync::mpsc::sync_channel(0);
    let server_thread = thread::spawn(move || {
        let journal = SwitchableJournal {
            available: journal_available,
            recorded: AtomicU64::new(0),
        };
        let mut durable = test_project();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let authority = control_server_identity(&control.borrow(), project_id);
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            address.ip(),
            CAPABILITIES_DIGEST,
        );
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();

        let (stream, _) = listener.accept().unwrap();
        handle_client(
            stream,
            &server,
            &control,
            &journal,
            &mut durable,
            &principal,
            &authority,
            None,
            None,
            &mut OnceClientOutcome::Unserved,
        )
        .unwrap();
        served_tx.send(()).unwrap();
    });

    let stream = TcpStream::connect(address).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let (mut reader, _) = complete_test_handshake(&stream);

    write_message(
        &mut writer,
        &WireMessage::Command(test_command(
            "durable-cut",
            "durable-cut-key",
            CommandPayload::Cut,
        )),
    )
    .unwrap();
    assert!(matches!(
        next_command_result(&mut reader),
        CommandResult::Accepted { revision: 1, .. }
    ));

    available.store(false, Ordering::SeqCst);
    for id in ["refused-first", "refused-second"] {
        write_message(
            &mut writer,
            &WireMessage::Command(test_command(id, &format!("{id}-key"), CommandPayload::Cut)),
        )
        .unwrap();
        match next_command_result(&mut reader) {
            CommandResult::Rejected {
                id: rejected,
                code,
                current_revision,
                retryable,
                ..
            } => {
                assert_eq!(rejected, id);
                assert_eq!(code, "unavailable");
                assert_eq!(current_revision, 1, "a refused command takes no revision");
                assert!(retryable);
            }
            CommandResult::Accepted { revision, .. } => {
                panic!("{id} must not be acknowledged, yet it took revision {revision}")
            }
        }
    }

    available.store(true, Ordering::SeqCst);
    write_message(
        &mut writer,
        &WireMessage::Command(test_command(
            "after-repair",
            "after-repair-key",
            CommandPayload::Cut,
        )),
    )
    .unwrap();
    assert!(
        matches!(
            next_command_result(&mut reader),
            CommandResult::Accepted { revision: 2, .. }
        ),
        "the refused commands left no gap in the revision or the journal"
    );
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    while reader
        .read_message_with_idle(|| Ok(false))
        .unwrap()
        .is_some()
    {}
    served_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    server_thread.join().unwrap();
}

#[test]
fn client_socket_configuration_sets_read_and_write_timeouts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let _client = TcpStream::connect(address).unwrap();
    let (server_stream, _) = listener.accept().unwrap();

    configure_client_socket(&server_stream).unwrap();

    // The kernel may round a socket timeout up to its timer granularity
    // (jiffies on Linux, for example 4 ms ticks), so each configured value
    // is bounded by the request plus one coarse tick rather than compared
    // exactly.
    let read_timeout = server_stream.read_timeout().unwrap().unwrap();
    assert!(read_timeout >= CLIENT_READ_POLL_INTERVAL);
    assert!(read_timeout <= CLIENT_READ_POLL_INTERVAL + Duration::from_millis(10));
    let write_timeout = server_stream.write_timeout().unwrap().unwrap();
    assert!(write_timeout >= CLIENT_WRITE_TIMEOUT);
    assert!(write_timeout <= CLIENT_WRITE_TIMEOUT + Duration::from_millis(10));
}

#[test]
fn client_write_timeouts_are_disconnects() {
    for kind in [std::io::ErrorKind::TimedOut, std::io::ErrorKind::WouldBlock] {
        let error = client_write_error(std::io::Error::from(kind));
        assert!(is_client_disconnect(error.as_ref()));
    }
}

fn complete_test_handshake(stream: &TcpStream) -> (MessageReader, ServerIdentity) {
    write_message(
        &mut stream.try_clone().unwrap(),
        &WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: PROTOCOL_VERSION,
            build: "control-timeout-test".into(),
            client_type: ClientType::Integration,
            desired_role: Role::Operator,
            resume_cursor: None,
        }),
    )
    .unwrap();
    let mut reader = MessageReader::new(stream.try_clone().unwrap());
    let Some(WireMessage::HandshakeResponse(response)) =
        reader.read_message_with_idle(|| Ok(false)).unwrap()
    else {
        panic!("expected handshake response");
    };
    assert!(matches!(
        reader.read_message_with_idle(|| Ok(false)).unwrap(),
        Some(WireMessage::Snapshot(_))
    ));
    (reader, response.server)
}

#[test]
fn incomplete_pre_handshake_peer_releases_accept_loop() {
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(100);

    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("show.freemix");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = std::sync::mpsc::sync_channel(1);
    let (expired_tx, expired_rx) = std::sync::mpsc::sync_channel(1);
    let server_thread = thread::spawn(move || {
        let store = ProjectStore::new(project_path).unwrap();
        let mut durable = test_project();
        store.save(&durable).unwrap();
        let journal = DurableJournal::open(&store).unwrap();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let authority = control_server_identity(&control.borrow(), project_id);
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            address.ip(),
            CAPABILITIES_DIGEST,
        )
        .with_session_limits(fm_server::SessionLimits {
            heartbeat_timeout_ms: u64::try_from(HANDSHAKE_TIMEOUT.as_millis()).unwrap(),
            ..fm_server::SessionLimits::default()
        });
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();

        for client_index in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            if client_index == 0 {
                accepted_tx.send(Instant::now()).unwrap();
            }
            handle_client(
                stream,
                &server,
                &control,
                &journal,
                &mut durable,
                &principal,
                &authority,
                None,
                None,
                &mut OnceClientOutcome::Unserved,
            )
            .unwrap();
            if client_index == 0 {
                expired_tx.send(Instant::now()).unwrap();
            }
        }
    });

    let mut incomplete_peer = TcpStream::connect(address).unwrap();
    let accepted_at = accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let deadline_lower_bound = accepted_at.checked_add(HANDSHAKE_TIMEOUT).unwrap();
    let mut wrote_partial_bytes = false;
    while Instant::now() < deadline_lower_bound {
        assert_eq!(incomplete_peer.write(b"x").unwrap(), 1);
        wrote_partial_bytes = true;
        thread::sleep(Duration::from_millis(10));
    }
    assert!(wrote_partial_bytes);
    let expired_at = expired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(expired_at >= deadline_lower_bound);
    let next_peer = TcpStream::connect(address).unwrap();
    next_peer
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    complete_test_handshake(&next_peer);

    drop(next_peer);
    server_thread.join().unwrap();
}

#[test]
fn outbound_rate_limited_client_releases_accept_loop() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("show.freemix");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (terminated_tx, terminated_rx) = std::sync::mpsc::sync_channel(1);
    let (completed_tx, completed_rx) = std::sync::mpsc::sync_channel(1);
    let server_thread = thread::spawn(move || {
        let store = ProjectStore::new(project_path).unwrap();
        let mut durable = test_project();
        store.save(&durable).unwrap();
        let journal = DurableJournal::open(&store).unwrap();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let authority = control_server_identity(&control.borrow(), project_id);
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            address.ip(),
            CAPABILITIES_DIGEST,
        )
        .with_session_limits(fm_server::SessionLimits {
            outbound_messages: fm_server::RateLimit::new(2, 60_000),
            ..fm_server::SessionLimits::default()
        });
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();

        let (stream, _) = listener.accept().unwrap();
        let error = handle_client(
            stream,
            &server,
            &control,
            &journal,
            &mut durable,
            &principal,
            &authority,
            None,
            None,
            &mut OnceClientOutcome::Unserved,
        )
        .unwrap_err();
        assert!(!is_client_disconnect(error.as_ref()));
        assert!(!is_client_protocol_error(error.as_ref()));
        assert!(is_client_session_termination(error.as_ref()));
        terminated_tx.send(()).unwrap();

        let (stream, _) = listener.accept().unwrap();
        handle_client(
            stream,
            &server,
            &control,
            &journal,
            &mut durable,
            &principal,
            &authority,
            None,
            None,
            &mut OnceClientOutcome::Unserved,
        )
        .unwrap();
        completed_tx.send(()).unwrap();
    });

    let client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (mut reader, mut server) = complete_test_handshake(&client);
    server.project_id = "wrong-project".into();
    write_message(
        &mut client.try_clone().unwrap(),
        &WireMessage::Heartbeat(HeartbeatMessage {
            server,
            sequence: 1,
            sent_at_ms: 1_234,
            last_applied: None,
        }),
    )
    .unwrap();
    terminated_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut eof = [0_u8; 1];
    assert_eq!(reader.stream.read(&mut eof).unwrap(), 0);
    drop(client);

    let next_client = TcpStream::connect(address).unwrap();
    next_client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    next_client
        .set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    complete_test_handshake(&next_client);
    drop(next_client);

    completed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    server_thread.join().unwrap();
}

#[test]
fn expired_tcp_session_is_reclaimed_for_next_client() {
    const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(600);
    const HEARTBEAT_DELAY: Duration = Duration::from_millis(200);
    // The server expires sessions on absolute wall-clock deadlines while
    // this measurement is monotonic, so a small system-clock step may
    // legitimately fire expiry early against this instant.
    const CLOCK_SKEW_TOLERANCE: Duration = Duration::from_millis(250);

    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("show.freemix");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (expired_tx, expired_rx) = std::sync::mpsc::sync_channel(1);
    let server_thread = thread::spawn(move || {
        let store = ProjectStore::new(project_path).unwrap();
        let mut durable = test_project();
        store.save(&durable).unwrap();
        let journal = DurableJournal::open(&store).unwrap();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let authority = control_server_identity(&control.borrow(), project_id);
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            address.ip(),
            CAPABILITIES_DIGEST,
        )
        .with_session_limits(fm_server::SessionLimits {
            heartbeat_timeout_ms: u64::try_from(HEARTBEAT_TIMEOUT.as_millis()).unwrap(),
            ..fm_server::SessionLimits::default()
        });
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();

        for client_index in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            handle_client(
                stream,
                &server,
                &control,
                &journal,
                &mut durable,
                &principal,
                &authority,
                None,
                None,
                &mut OnceClientOutcome::Unserved,
            )
            .unwrap();
            if client_index == 0 {
                expired_tx.send(()).unwrap();
            }
        }
    });

    let client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (mut reader, server) = complete_test_handshake(&client);
    let original_deadline = Instant::now() + HEARTBEAT_TIMEOUT;
    assert!(matches!(
        expired_rx.recv_timeout(HEARTBEAT_DELAY),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));

    let heartbeat_sent_at = Instant::now();
    write_message(
        &mut client.try_clone().unwrap(),
        &WireMessage::Heartbeat(HeartbeatMessage {
            server: server.clone(),
            sequence: 1,
            sent_at_ms: 1_234,
            last_applied: None,
        }),
    )
    .unwrap();
    let Some(WireMessage::HeartbeatAcknowledgement(acknowledgement)) =
        reader.read_message_with_idle(|| Ok(false)).unwrap()
    else {
        panic!("expected heartbeat acknowledgement");
    };
    assert_eq!(acknowledgement.server, server);
    assert_eq!(acknowledgement.heartbeat_sequence, 1);

    assert!(matches!(
        expired_rx.recv_timeout(original_deadline.saturating_duration_since(Instant::now())),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(Instant::now() >= original_deadline);
    expired_rx.recv_timeout(HEARTBEAT_TIMEOUT).unwrap();
    assert!(
        heartbeat_sent_at.elapsed() + CLOCK_SKEW_TOLERANCE >= HEARTBEAT_TIMEOUT,
        "the session expired before its heartbeat deadline"
    );

    let next_client = TcpStream::connect(address).unwrap();
    next_client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    complete_test_handshake(&next_client);
    drop(next_client);

    server_thread.join().unwrap();
}

#[cfg(feature = "native-media")]
#[test]
fn recorder_capture_failure_policy_formats_one_sanitized_notice() {
    let mut policy = RecorderCapturePolicy::default();
    let Some((failure, app_capture_failure)) =
        policy.fail("readback:first\tline\nend".to_owned(), true)
    else {
        panic!("first failure must emit a notice");
    };
    assert_eq!(
        recorder_failure_notice(2, failure, app_capture_failure),
        "FREEMIXD_RECORDER_FAILURE\tv=1\tslot=2\tapp_capture_failure=true\tfailure=readback:first line end",
    );
    assert!(policy.fail("enqueue:second".to_owned(), false).is_none());
}

#[cfg(feature = "native-media")]
#[test]
fn recorder_report_annotates_the_slot_immediately_after_the_version() {
    let report = StopReport {
        outcome: StopOutcome::Clean,
        exit_status: Some(0),
        output: OutputFinalization::Synced,
        cleanup: CleanupStatus::Complete,
        wav_bytes: 0,
        telemetry: fm_codec_ffmpeg::record::RecordTelemetry {
            state: RecorderState::Stopped,
            failure: None,
            accepted_pairs: 30,
            completed_pairs: 30,
            failed_pairs: 0,
            outstanding_pairs: 0,
            retained_bytes: 0,
            output_bytes: 1_024,
            wav_bytes: 512,
            stderr_tail: String::new(),
            stderr_truncated: false,
        },
    };
    assert_eq!(
        recorder_report(
            1,
            &report,
            1_000,
            2_048,
            RecorderBacklog::default(),
            false,
            "none",
            None,
            None
        ),
        "FREEMIXD_RECORDER\tv=1\tslot=1\tstate=Stopped\toutcome=Clean\taccepted_pairs=30\tcompleted_pairs=30\toutput_bytes=1024\twav_bytes=512\tvideo_bitrate_kbps=unset\taudio_bitrate_kbps=unset\tduration_ms=1000\tfree_bytes=2048\toutput_finalization=Synced\tcleanup=Complete\tbacklogged_pairs=0\tbacklog_peak_bytes=0\tapp_capture_failure=false\tfailure=none",
    );
    // The second slot is annotated by number, never by position in a
    // combined record, so a mixed run keeps the two segments separable.
    assert!(
        recorder_report(
            2,
            &report,
            0,
            0,
            RecorderBacklog::default(),
            false,
            "none",
            None,
            None
        )
        .starts_with("FREEMIXD_RECORDER\tv=1\tslot=2\tstate=Stopped\t")
    );
}

#[cfg(feature = "native-media")]
#[test]
fn recorder_report_renders_configured_and_unset_bitrates() {
    let report = StopReport {
        outcome: StopOutcome::Clean,
        exit_status: Some(0),
        output: OutputFinalization::Synced,
        cleanup: CleanupStatus::Complete,
        wav_bytes: 0,
        telemetry: fm_codec_ffmpeg::record::RecordTelemetry {
            state: RecorderState::Stopped,
            failure: None,
            accepted_pairs: 1,
            completed_pairs: 1,
            failed_pairs: 0,
            outstanding_pairs: 0,
            retained_bytes: 0,
            output_bytes: 128,
            wav_bytes: 0,
            stderr_tail: String::new(),
            stderr_truncated: false,
        },
    };
    assert!(
        recorder_report(
            1,
            &report,
            0,
            0,
            RecorderBacklog::default(),
            false,
            "none",
            Some(2_000),
            Some(96)
        )
        .contains("\twav_bytes=0\tvideo_bitrate_kbps=2000\taudio_bitrate_kbps=96\tduration_ms=0\t")
    );
    assert!(
        recorder_report(
            2,
            &report,
            0,
            0,
            RecorderBacklog::default(),
            false,
            "none",
            None,
            None
        )
        .contains("\twav_bytes=0\tvideo_bitrate_kbps=unset\taudio_bitrate_kbps=unset\t")
    );
    // Each field is independent: one slot can set video while keeping the
    // encoder's default audio.
    assert!(
        recorder_report(
            2,
            &report,
            0,
            0,
            RecorderBacklog::default(),
            false,
            "none",
            Some(1_000),
            None
        )
        .contains("\tvideo_bitrate_kbps=1000\taudio_bitrate_kbps=unset\t")
    );
}

#[cfg(feature = "native-media")]
#[test]
fn recording_output_requires_mp4_and_existing_directory_parent() {
    let directory = tempfile::tempdir().unwrap();
    assert!(
        record_output_parent(&directory.path().join("capture.mov"))
            .unwrap_err()
            .to_string()
            .contains("final extension `.mp4`")
    );
    assert!(
        record_output_parent(&directory.path().join("missing/capture.mp4"))
            .unwrap_err()
            .to_string()
            .contains("existing canonical directory")
    );
    let not_directory = directory.path().join("parent-file");
    fs::write(&not_directory, b"parent").unwrap();
    assert!(
        record_output_parent(&not_directory.join("capture.mp4"))
            .unwrap_err()
            .to_string()
            .contains("existing canonical directory")
    );
    assert_eq!(
        record_output_parent(&directory.path().join("capture.mp4")).unwrap(),
        fs::canonicalize(directory.path()).unwrap()
    );
}

#[cfg(feature = "native-media")]
fn planned_session(
    path: &Path,
    names: &recording_session::SegmentNames,
) -> recording_session::PlannedSession {
    let parent = record_output_parent(path).unwrap();
    recording_session::plan(path, &parent, names, 1, 1_000_000, None).unwrap()
}
