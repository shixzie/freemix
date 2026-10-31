//! Imports an M3U/M3U8 playlist into a saved media list (IN-006).
//!
//! Every entry is a local file, resolved against the playlist's directory.
//! All files are checked before anything is copied, then imported into the
//! bundle like `project-import-media`, and appended to the list's existing
//! items in playlist order. Titles come from `#EXTINF`; without one the
//! source file's stem is used.

use std::{fs, path::Path};

use fm_model::{MAX_MEDIA_LIST_ITEM_ID, MAX_MEDIA_LIST_ITEMS, MediaListItem};
use fm_persistence::ProjectStore;

use crate::app::{
    AppFailure, AppResult, input_id, inspect_engine, load_stored_project, mutate,
};

/// Imports `playlist` into media list `input` of the bundle at `path`.
pub fn import(path: &Path, input: u128, playlist: &Path) -> AppResult<()> {
    load_stored_project(path)?;
    let input = input_id(input)?;
    let engine = inspect_engine(path)?;
    let list = engine
        .engine
        .show()
        .media_list(input)
        .cloned()
        .ok_or_else(|| AppFailure(format!("input {input} is not a media list")))?;

    let bytes = fs::read(playlist).map_err(|error| {
        AppFailure(format!(
            "playlist {} could not be read: {error}",
            playlist.display()
        ))
    })?;
    let entries = fm_playback::parse_m3u(&bytes)
        .map_err(|error| AppFailure(format!("playlist {}: {error}", playlist.display())))?;
    let appended = entries.len();
    let remaining = MAX_MEDIA_LIST_ITEMS.saturating_sub(list.items.len());
    if appended > remaining {
        return Err(AppFailure(format!(
            "playlist names {appended} files; media list {input} has room for {remaining} more \
             (maximum {MAX_MEDIA_LIST_ITEMS} items)"
        ))
        .into());
    }

    let base = playlist.parent().unwrap_or_else(|| Path::new("."));
    let mut files = Vec::with_capacity(appended);
    for entry in entries {
        let named = Path::new(&entry.path);
        let file = if named.is_absolute() {
            named.to_path_buf()
        } else {
            base.join(named)
        };
        if !file.is_file() {
            return Err(AppFailure(format!(
                "playlist {} names {}, which is not a file",
                playlist.display(),
                file.display()
            ))
            .into());
        }
        files.push((entry.title, file));
    }

    let store = ProjectStore::new(path)?;
    let mut items = list.items.clone();
    let mut next = items
        .iter()
        .map(|item| item.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|id| *id <= MAX_MEDIA_LIST_ITEM_ID)
        .ok_or_else(|| AppFailure(format!("media list {input} has no free item id")))?;
    for (title, file) in files {
        let imported = store
            .import_media_file(&file)
            .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
        let title = title.or_else(|| {
            file.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        });
        println!(
            "media-list-import: {} -> {}",
            file.display(),
            imported.asset_uri
        );
        items.push(MediaListItem {
            id: next,
            asset_uri: imported.asset_uri,
            title,
            mark_in: 0,
            mark_out: None,
        });
        next = next
            .checked_add(1)
            .filter(|id| *id <= MAX_MEDIA_LIST_ITEM_ID)
            .ok_or_else(|| AppFailure(format!("media list {input} has no free item id")))?;
    }

    mutate(
        path,
        fm_engine::EngineCommand::MediaList(fm_engine::EngineMediaListCommand {
            input,
            operation: fm_engine::EngineMediaListOperation::Set {
                items,
                options: list.options,
                active: Some(list.active),
            },
        }),
        1,
        None,
        None,
    )?;
    println!(
        "input={input} items={} added={appended}",
        list.items.len() + appended
    );
    Ok(())
}
