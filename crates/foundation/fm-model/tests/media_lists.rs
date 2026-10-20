//! IN-006 in the model: media list bounds, the project invariant that a list
//! input plays its active item's file, and M3U parsing.

use core::num::NonZeroU128;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fm_model::{
    Input, InputKind, M3uEntry, MAX_MEDIA_LIST_ITEMS, MediaList, MediaListError, MediaListItem,
    MediaListOptions, MediaPlaybackSettings, Project, ProjectSettings, is_m3u_file_name,
    is_media_list_file_name, parse_m3u,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn item(id: u32, file: &str) -> MediaListItem {
    MediaListItem::new(id, format!("asset://media/{file}"))
}

fn project_with(asset: &str, list: MediaList) -> Project {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(1).unwrap()),
        "Show",
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
    );
    project.add_input(Input {
        id: input(1),
        name: "List".into(),
        kind: InputKind::Media {
            asset_uri: asset.into(),
            playback: MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
    project.set_media_lists(BTreeMap::from([(input(1), list)]));
    project
}

#[test]
fn a_new_list_starts_on_its_first_item_in_play_order() {
    let list = MediaList::new(
        vec![item(4, "a.mp4"), item(9, "b.wav"), item(2, "c.mkv")],
        MediaListOptions::default(),
    )
    .unwrap();
    assert_eq!(list.active, 4);
    assert_eq!(list.play_order(), [4, 9, 2]);
    assert_eq!(list.position(), Some(0));
    assert_eq!(list.next_item_id(), 10);
    let shuffled = MediaList::new(
        list.items.clone(),
        MediaListOptions {
            shuffle_seed: Some(11),
            ..MediaListOptions::default()
        },
    )
    .unwrap();
    assert_eq!(shuffled.active, shuffled.play_order()[0]);
    assert_eq!(shuffled.position(), Some(0));
}

#[test]
fn items_are_bounded() {
    let new = |items| MediaList::new(items, MediaListOptions::default()).map(|_| ());
    assert_eq!(new(Vec::new()), Err(MediaListError::Empty));
    assert_eq!(
        new(vec![item(0, "a.mp4")]),
        Err(MediaListError::InvalidItemId(0))
    );
    assert_eq!(
        new(vec![item(1, "a.png")]),
        Err(MediaListError::UnsupportedType(1))
    );
    assert_eq!(
        new(vec![MediaListItem::new(1, "/abs/a.mp4")]),
        Err(MediaListError::InvalidAsset(1))
    );
    let mut titled = item(1, "a.mp4");
    titled.title = Some("bad\ntitle".into());
    assert_eq!(new(vec![titled]), Err(MediaListError::InvalidTitle(1)));
    let mut marked = item(1, "a.mp4");
    marked.mark_in = 9;
    marked.mark_out = Some(9);
    assert_eq!(new(vec![marked]), Err(MediaListError::InvalidMarks(1)));
    let many = (1..=u32::try_from(MAX_MEDIA_LIST_ITEMS).unwrap() + 1)
        .map(|id| item(id, "a.mp4"))
        .collect();
    assert!(matches!(
        new(many),
        Err(MediaListError::TooManyItems { .. })
    ));
    assert!(is_media_list_file_name("music/bed.FLAC"));
    assert!(!is_media_list_file_name("slides/1.jpeg"));
}

#[test]
fn a_project_list_input_plays_its_active_item() {
    let list = MediaList::new(
        vec![item(1, "a.mp4"), item(2, "b.mp4")],
        MediaListOptions::default(),
    )
    .unwrap();
    assert!(
        project_with("asset://media/a.mp4", list.clone())
            .validate()
            .is_ok()
    );
    let errors = project_with("asset://media/b.mp4", list.clone())
        .validate()
        .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "media_lists.active"),
        "{errors:?}"
    );
    // Removing the input drops its list.
    let mut project = project_with("asset://media/a.mp4", list);
    project.remove_input(input(1)).unwrap();
    assert!(project.media_lists().is_empty());
}

#[test]
fn m3u_entries_resolve_relative_paths_and_take_extinf_titles() {
    let text = "\u{feff}#EXTM3U\r\n\
        #EXTINF:12,Opening Titles\r\n\
        intro/opening.mp4\r\n\
        \r\n\
        # a comment\r\n\
        #EXTINF:-1 tvg-id=\"x\",Live Bed\r\n\
        /srv/media/bed.wav\r\n\
        file:///srv/media/with%20space.mkv\r\n\
        https://example.com/stream.m3u8\r\n\
        stills/logo.png\r\n\
        #EXTINF:3.9,\r\n\
        ../outro.mov\r\n";
    let base = Path::new("/shows/demo");
    assert_eq!(
        parse_m3u(text, base),
        [
            M3uEntry {
                path: PathBuf::from("/shows/demo/intro/opening.mp4"),
                title: Some("Opening Titles".into()),
                duration_seconds: Some(12),
            },
            M3uEntry {
                path: PathBuf::from("/srv/media/bed.wav"),
                title: Some("Live Bed".into()),
                duration_seconds: None,
            },
            M3uEntry {
                path: PathBuf::from("/srv/media/with space.mkv"),
                title: None,
                duration_seconds: None,
            },
            M3uEntry {
                path: PathBuf::from("/shows/demo/../outro.mov"),
                title: None,
                duration_seconds: Some(3),
            },
        ]
    );
    assert!(is_m3u_file_name("Show.M3U8"));
    assert!(!is_m3u_file_name("show.mp4"));
}

#[test]
fn an_extinf_title_applies_only_to_the_next_entry_and_is_bounded() {
    let long = "x".repeat(400);
    let text = format!("#EXTINF:5,{long}\na.mp4\nb.mp4\n");
    let entries = parse_m3u(&text, Path::new("/m"));
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].title.as_ref().map(String::len), Some(128));
    assert_eq!(entries[1].title, None);
}
