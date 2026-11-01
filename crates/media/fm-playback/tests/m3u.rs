//! Bounded M3U media-list parser tests (IN-006).

use fm_playback::{
    M3uEntry, M3uError, MAX_M3U_BYTES, MAX_M3U_ENTRIES, MAX_M3U_PATH_BYTES, MAX_M3U_TITLE_BYTES,
    parse_m3u,
};

#[test]
fn parses_a_plain_file_list_with_comments_and_blank_lines() {
    let entries =
        parse_m3u(b"\n#EXTM3U\nfirst.mp4\n\n  folder/second.mkv  \r\nthird.wav\n").unwrap();
    assert_eq!(
        entries,
        vec![
            M3uEntry {
                path: "first.mp4".into(),
                title: None
            },
            M3uEntry {
                path: "folder/second.mkv".into(),
                title: None
            },
            M3uEntry {
                path: "third.wav".into(),
                title: None
            },
        ]
    );
}

#[test]
fn extinf_titles_attach_to_the_next_path_and_a_bom_is_ignored() {
    let document = "\u{feff}#EXTM3U\r\n#EXTINF:12, Opening clip \r\nopening.mov\r\n# a comment\r\n#EXTINF:-1\r\nuntitled.mp4\r\n";
    let entries = parse_m3u(document.as_bytes()).unwrap();
    assert_eq!(entries[0].title.as_deref(), Some("Opening clip"));
    assert_eq!(entries[0].path, "opening.mov");
    assert_eq!(entries[1].title, None);
    assert_eq!(entries[1].path, "untitled.mp4");
}

#[test]
fn extinf_attributes_before_the_comma_are_ignored() {
    let entries = parse_m3u(b"#EXTINF:-1 tvg-id=\"news\",News at nine\nnews.mp4\n").unwrap();
    assert_eq!(entries[0].title.as_deref(), Some("News at nine"));
}

#[test]
fn a_url_entry_is_refused() {
    assert_eq!(
        parse_m3u(b"https://example.com/stream.m3u8\n").unwrap_err(),
        M3uError::UrlUnsupported { line: 1 }
    );
}

#[test]
fn an_empty_playlist_is_refused() {
    assert_eq!(parse_m3u(b"#EXTM3U\n\n").unwrap_err(), M3uError::Empty);
}

#[test]
fn a_trailing_extinf_without_a_path_is_refused() {
    assert_eq!(
        parse_m3u(b"clip.mp4\n#EXTINF:1,Unused\n").unwrap_err(),
        M3uError::MissingPath { line: 2 }
    );
}

#[test]
fn bounds_are_enforced() {
    let mut too_many = String::new();
    for index in 0..=MAX_M3U_ENTRIES {
        use std::fmt::Write as _;
        writeln!(too_many, "{index}.mp4").unwrap();
    }
    assert_eq!(
        parse_m3u(too_many.as_bytes()).unwrap_err(),
        M3uError::TooManyEntries {
            count: MAX_M3U_ENTRIES + 1
        }
    );

    let long_path = format!("{}\n", "a".repeat(MAX_M3U_PATH_BYTES + 1));
    assert_eq!(
        parse_m3u(long_path.as_bytes()).unwrap_err(),
        M3uError::PathTooLong {
            line: 1,
            bytes: MAX_M3U_PATH_BYTES + 1
        }
    );

    let long_title = format!("#EXTINF:1,{}\nx.mp4\n", "t".repeat(MAX_M3U_TITLE_BYTES + 1));
    assert_eq!(
        parse_m3u(long_title.as_bytes()).unwrap_err(),
        M3uError::TitleTooLong {
            line: 1,
            bytes: MAX_M3U_TITLE_BYTES + 1
        }
    );

    let too_large = vec![b'a'; MAX_M3U_BYTES + 1];
    assert_eq!(
        parse_m3u(&too_large).unwrap_err(),
        M3uError::TooLarge {
            bytes: MAX_M3U_BYTES + 1
        }
    );
}

#[test]
fn control_characters_and_invalid_utf8_are_refused() {
    assert_eq!(
        parse_m3u(b"bad\x07path.mp4\n").unwrap_err(),
        M3uError::ControlCharacter { line: 1 }
    );
    assert_eq!(
        parse_m3u(&[0xff, 0xfe, b'\n']).unwrap_err(),
        M3uError::InvalidUtf8
    );
}
