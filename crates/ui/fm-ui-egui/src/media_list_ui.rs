//! Media list inputs in Studio (IN-006): the Add input form that creates
//! one from pasted file paths and M3U playlists, and the Media tab's list
//! card under the input's transport bar.
//!
//! The card shows the items in list order with the active one highlighted
//! and the play position, a search filter, Previous and Next, a key that
//! makes any item active, drag (or Up / Down) reordering, per-item in and
//! out marks, removal, appending files, and the Auto Next, Auto First,
//! Loop, and Shuffle options. Stepping through the list is an operator
//! control (`can_transition`); every other change is a project edit sent as
//! one `media_list` `set` with the whole list, padlocks honoured.

use std::path::Path;

use egui::{DragValue, Id, RichText, Sense, Stroke, Ui, Vec2};
use fm_protocol::{
    MAX_MEDIA_LIST_ITEMS, MAX_MEDIA_LIST_TEXT_BYTES, MediaListItemSpec, MediaListNavigation,
    MediaListOperation, MediaListOptionsSpec, MediaListStatus,
};
use fm_types::InputId;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_lifecycle::parse_import_paths;
use crate::l10n::{Arg, Locale, Msg};
use crate::media_transport::media_settings_block;
use crate::playlist::key_button;
use crate::theme::{
    ACCENT_FILL, KeyIcon, PROGRAM, Tone, accent_color, add_icon_button, caption, section_label,
    small_button, status_chip, text_color, text_dim_color,
};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

/// The width of a row's icon keys.
const ROW_KEY_WIDTH: f32 = 26.0;

/// The longest path one item may name, as the wire bounds an input source.
const MAX_ITEM_PATH_BYTES: usize = 4096;

/// The Add input dialog's media list form.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // the list's four option checkboxes
pub(crate) struct MediaListDraft {
    /// One video or audio file, or `.m3u` / `.m3u8` playlist, per line.
    pub(crate) text: String,
    pub(crate) auto_next: bool,
    pub(crate) auto_first: bool,
    pub(crate) loop_list: bool,
    pub(crate) shuffle: bool,
}

impl Default for MediaListDraft {
    fn default() -> Self {
        Self {
            text: String::new(),
            auto_next: true,
            auto_first: false,
            loop_list: false,
            shuffle: false,
        }
    }
}

impl MediaListDraft {
    /// The list's items, read from the pasted lines (playlists expanded),
    /// or why they cannot be.
    pub(crate) fn items(&self) -> Result<Vec<MediaListItemSpec>, &'static str> {
        expand_items(&self.text, &[], &read_playlist)
    }

    /// The list's options; a shuffled list's seed follows its files.
    pub(crate) fn options(&self, items: &[MediaListItemSpec]) -> MediaListOptionsSpec {
        MediaListOptionsSpec {
            auto_next: self.auto_next,
            auto_first: self.auto_first,
            loop_list: self.loop_list,
            shuffle_seed: self.shuffle.then(|| shuffle_seed(items, 0)),
        }
    }
}

/// Reads an M3U playlist from this computer, at most its size bound.
fn read_playlist(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if usize::try_from(metadata.len()).ok()? > fm_types::media_list_files::MAX_M3U_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Expands pasted lines into list items numbered with the smallest ids not
/// in `taken` (the list's own), in order: a media file is one item; an M3U playlist (read with `read`) is each of its
/// media entries, relative ones resolved against the playlist's folder and
/// titled by `#EXTINF`.
///
/// # Errors
///
/// The reason the lines make no list: none, too many, a line that is not a
/// media file or playlist, a playlist that cannot be read, a file that is
/// neither an absolute path nor an `asset://` URI, or a path or list text
/// beyond the wire bounds.
pub(crate) fn expand_items(
    text: &str,
    taken: &[u32],
    read: &dyn Fn(&Path) -> Option<String>,
) -> Result<Vec<MediaListItemSpec>, &'static str> {
    let mut files: Vec<(String, Option<String>)> = Vec::new();
    for line in parse_import_paths(text) {
        if fm_types::media_list_files::is_m3u_file_name(&line) {
            let path = Path::new(&line);
            let playlist = read(path).ok_or(Msg::MlReasonPlaylistUnreadable.en())?;
            let base = path.parent().unwrap_or_else(|| Path::new(""));
            for entry in fm_types::media_list_files::parse_m3u(
                &playlist,
                base,
                MAX_MEDIA_LIST_ITEMS,
                fm_protocol::MAX_MEDIA_LIST_TITLE_BYTES,
            ) {
                let file = entry.path.to_string_lossy().into_owned();
                files.push((file, entry.title));
            }
        } else if fm_types::media_list_files::is_media_list_file_name(&line) {
            files.push((line, None));
        } else {
            return Err(Msg::MlReasonNotMedia.en());
        }
    }
    if files.is_empty() {
        return Err(Msg::MlReasonNoItems.en());
    }
    if files.len() > MAX_MEDIA_LIST_ITEMS {
        return Err(Msg::MlReasonTooManyItems.en());
    }
    if files
        .iter()
        .any(|(file, _)| file.len() > MAX_ITEM_PATH_BYTES)
    {
        return Err(Msg::LcReasonPathTooLong.en());
    }
    // The server copies an absolute path on its computer into the project;
    // anything else but an `asset://` URI already there it refuses.
    if files
        .iter()
        .any(|(file, _)| !fm_types::still::request_uri_is_valid(file, MAX_ITEM_PATH_BYTES))
    {
        return Err(Msg::MlReasonPathInvalid.en());
    }
    let items: Vec<MediaListItemSpec> = files
        .into_iter()
        .zip(free_item_ids(taken))
        .map(|((uri, title), id)| MediaListItemSpec {
            id,
            uri,
            title: title.map(|title| truncated(&title, fm_protocol::MAX_MEDIA_LIST_TITLE_BYTES)),
            mark_in: 0,
            mark_out: None,
        })
        .collect();
    if text_bytes(&items) > MAX_MEDIA_LIST_TEXT_BYTES {
        return Err(Msg::MlReasonTooMuchText.en());
    }
    Ok(items)
}

/// The item ids not in `taken`, ascending: there are always more than a
/// list can hold, since `MAX_MEDIA_LIST_ITEM_ID` far exceeds
/// `MAX_MEDIA_LIST_ITEMS`.
fn free_item_ids(taken: &[u32]) -> impl Iterator<Item = u32> + '_ {
    (1..=fm_protocol::MAX_MEDIA_LIST_ITEM_ID).filter(move |id| !taken.contains(id))
}

/// `text` cut to at most `bytes` bytes on a character boundary.
fn truncated(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The files' and titles' bytes, which the wire bounds per list.
fn text_bytes(items: &[MediaListItemSpec]) -> usize {
    items
        .iter()
        .map(|item| item.uri.len() + item.title.as_ref().map_or(0, String::len))
        .sum()
}

/// A shuffle seed derived from the files and a `salt`, so the same list
/// shuffles the same way and a reshuffle (another salt) another way.
#[must_use]
pub(crate) fn shuffle_seed(items: &[MediaListItemSpec], salt: u64) -> u64 {
    // FNV-1a over the files: deterministic across runs and platforms.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325 ^ salt;
    for item in items {
        for byte in item.uri.bytes().chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// The display name of an item: its title, else its file name.
#[must_use]
pub(crate) fn item_name(item: &MediaListItemSpec) -> &str {
    item.title.as_deref().unwrap_or_else(|| {
        let uri = item.uri.as_str();
        uri.rsplit(['/', '\\']).next().unwrap_or(uri)
    })
}

/// The indices of the items whose name or file contains `query`, ignoring
/// case; every item for a blank query.
#[must_use]
pub(crate) fn filtered_items(items: &[MediaListItemSpec], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            query.is_empty()
                || item_name(item).to_lowercase().contains(&query)
                || item.uri.to_lowercase().contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

/// `items` with the item at `from` moved to `to` (both list positions).
#[must_use]
pub(crate) fn moved(items: &[MediaListItemSpec], from: usize, to: usize) -> Vec<MediaListItemSpec> {
    let mut items = items.to_vec();
    if from < items.len() && to < items.len() {
        let item = items.remove(from);
        items.insert(to, item);
    }
    items
}

/// The options of `status` as a `set` sends them.
const fn options_of(status: &MediaListStatus) -> MediaListOptionsSpec {
    status.options
}

/// Why the list's operator keys are unavailable, if they are.
fn control_block(state: &StudioUiState) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_transition;
    (!ready).then(|| unavailable_reason(state, Capability::Transition))
}

/// The list card of media list input `input`, under its transport bar.
#[allow(clippy::too_many_lines)] // one card
pub(crate) fn draw_list(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    input: InputId,
    list: &MediaListStatus,
    intents: &mut Vec<StudioIntent>,
) {
    let control = control_block(state);
    let edit = media_settings_block(state, input);
    let on_air = state
        .view
        .as_ref()
        .is_some_and(|view| crate::tally_state(input, view.switcher).is_realized_program());
    let mut operation = None;
    let set =
        |items: Vec<MediaListItemSpec>, options: MediaListOptionsSpec| MediaListOperation::Set {
            items,
            options,
            active: None,
        };

    ui.add_space(4.0);
    ui.separator();
    // Title, position, Previous and Next.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(section_label(l.text(Msg::MlTitle)));
        ui.add_space(4.0);
        ui.label(
            caption(l.format(
                Msg::MlPosition,
                &[
                    ("position", Arg::from(list.position as usize + 1)),
                    ("count", Arg::from(list.items.len())),
                ],
            ))
            .color(text_dim_color()),
        )
        .on_hover_text(l.text(Msg::MlPositionHover));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (navigation, label, hover) in [
                (
                    MediaListNavigation::Next,
                    Msg::PlaylistNext,
                    Msg::MlNextHover,
                ),
                (
                    MediaListNavigation::Previous,
                    Msg::PlaylistPrevious,
                    Msg::MlPreviousHover,
                ),
            ] {
                if ui
                    .add_enabled(
                        control.is_none(),
                        key_button(&l.text(label).to_uppercase(), None, 72.0),
                    )
                    .hint(l.text(hover), control.unwrap_or(""))
                    .clicked()
                {
                    operation = Some(MediaListOperation::Navigate(navigation));
                }
            }
        });
    });

    // Options.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let options = options_of(list);
        let mut toggles = [
            (options.auto_next, Msg::MlAutoNext, Msg::MlAutoNextHover),
            (options.auto_first, Msg::MlAutoFirst, Msg::MlAutoFirstHover),
            (options.loop_list, Msg::MlLoop, Msg::MlLoopHover),
            (
                options.shuffle_seed.is_some(),
                Msg::MlShuffle,
                Msg::MlShuffleHover,
            ),
        ];
        let mut changed = None;
        for (index, (value, label, hover)) in toggles.iter_mut().enumerate() {
            if ui
                .add_enabled(edit.is_none(), egui::Checkbox::new(value, l.text(*label)))
                .hint(l.text(*hover), edit.unwrap_or(""))
                .changed()
            {
                changed = Some(index);
            }
        }
        if let Some(index) = changed {
            let mut next = options;
            match index {
                0 => next.auto_next = toggles[0].0,
                1 => next.auto_first = toggles[1].0,
                2 => next.loop_list = toggles[2].0,
                _ => {
                    next.shuffle_seed = toggles[3].0.then(|| shuffle_seed(&list.items, list.cycle));
                }
            }
            operation = Some(set(list.items.clone(), next));
        }
        if let Some(seed) = options.shuffle_seed
            && ui
                .add_enabled(edit.is_none(), small_button(l.text(Msg::MlReshuffle)))
                .hint(l.text(Msg::MlReshuffleHover), edit.unwrap_or(""))
                .clicked()
        {
            let mut next = options;
            next.shuffle_seed = Some(shuffle_seed(&list.items, seed.wrapping_add(1)));
            operation = Some(set(list.items.clone(), next));
        }
    });

    // Search.
    let query_id = Id::new(("studio-media-list-search", input));
    let mut query = ui
        .data(|data| data.get_temp::<String>(query_id))
        .unwrap_or_default();
    let search = ui
        .add(
            egui::TextEdit::singleline(&mut query)
                .id(Id::new(("studio-media-list-search-field", input)))
                .hint_text(l.text(Msg::MlSearchHint))
                .desired_width(ui.available_width()),
        )
        .on_hover_text(l.text(Msg::MlSearchHover));
    if search.changed() {
        ui.data_mut(|data| data.insert_temp(query_id, query.clone()));
    }

    // Rows.
    let shown = filtered_items(&list.items, &query);
    if shown.is_empty() {
        ui.label(caption(l.text(Msg::MlNoMatches)));
    }
    let final_row = list.items.len() - 1;
    for index in shown {
        let item = &list.items[index];
        let active = item.id == list.active;
        // The current item is red only while the list is on Program; cued
        // off air it is the console's selection blue.
        let (fill, stroke) = match (active, on_air) {
            (false, _) => (egui::Color32::TRANSPARENT, Stroke::NONE),
            (true, true) => (
                PROGRAM.gamma_multiply(0.16),
                Stroke::new(1.0, PROGRAM.gamma_multiply(0.8)),
            ),
            (true, false) => (ACCENT_FILL, Stroke::new(1.0, accent_color())),
        };
        let row = egui::Frame::new()
            .fill(fill)
            .stroke(stroke)
            .corner_radius(egui::CornerRadius::same(crate::theme::RADIUS))
            .inner_margin(egui::Margin::symmetric(4, 2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // The name is the drag handle.
                    let drag = ui
                        .dnd_drag_source(
                            Id::new(("studio-media-list-row", input, item.id)),
                            index,
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(item_name(item)).strong().color(text_color()),
                                    )
                                    .truncate()
                                    .sense(Sense::hover()),
                                );
                            },
                        )
                        .response;
                    drag.on_hover_text(if edit.is_none() {
                        l.text(Msg::MlDragHover).to_owned()
                    } else {
                        crate::l10n::disabled_text(l, l.text(Msg::MlDragHover), edit.unwrap_or(""))
                    });
                    if active {
                        let tone = if on_air { Tone::OnAir } else { Tone::Ready };
                        status_chip(ui, l.text(Msg::MlActive), tone, 64.0)
                            .on_hover_text(l.text(Msg::MlActiveHover));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let only = list.items.len() == 1;
                        if add_icon_button(
                            ui,
                            edit.is_none() && !only,
                            key_button("", None, ROW_KEY_WIDTH),
                            KeyIcon::Remove,
                            l.text(Msg::MlRemoveHover),
                        )
                        .hint(
                            l.text(Msg::MlRemoveHover),
                            edit.unwrap_or(Msg::MlReasonOnlyItem.en()),
                        )
                        .clicked()
                        {
                            let mut items = list.items.clone();
                            items.remove(index);
                            operation = Some(set(items, options_of(list)));
                        }
                        for (icon, hover, possible, to, end) in [
                            (
                                KeyIcon::Down,
                                Msg::MlDownHover,
                                index < final_row,
                                index + 1,
                                Msg::MlReasonLastItem,
                            ),
                            (
                                KeyIcon::Up,
                                Msg::MlUpHover,
                                index > 0,
                                index.wrapping_sub(1),
                                Msg::MlReasonFirstItem,
                            ),
                        ] {
                            if add_icon_button(
                                ui,
                                edit.is_none() && possible,
                                key_button("", None, ROW_KEY_WIDTH),
                                icon,
                                l.text(hover),
                            )
                            .hint(l.text(hover), edit.unwrap_or(end.en()))
                            .clicked()
                            {
                                operation =
                                    Some(set(moved(&list.items, index, to), options_of(list)));
                            }
                        }
                        if add_icon_button(
                            ui,
                            control.is_none() && !active,
                            key_button("", None, ROW_KEY_WIDTH),
                            KeyIcon::Play,
                            l.text(Msg::MlActivateHover),
                        )
                        .hint(
                            l.text(Msg::MlActivateHover),
                            control.unwrap_or(Msg::MlReasonAlreadyActive.en()),
                        )
                        .clicked()
                        {
                            operation = Some(MediaListOperation::Navigate(
                                MediaListNavigation::Item(item.id),
                            ));
                        }
                    });
                });
                // Marks: in, and out (zero plays to the file's end).
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let in_id = Id::new(("studio-media-list-in", input, item.id));
                    let out_id = Id::new(("studio-media-list-out", input, item.id));
                    let mut mark_in = ui
                        .data(|data| data.get_temp::<u64>(in_id))
                        .unwrap_or(item.mark_in);
                    let mut mark_out = ui
                        .data(|data| data.get_temp::<u64>(out_id))
                        .unwrap_or(item.mark_out.unwrap_or(0));
                    ui.label(caption(l.text(Msg::MediaTrimIn)));
                    let in_response = ui
                        .add_enabled(edit.is_none(), DragValue::new(&mut mark_in).speed(1.0))
                        .hint(l.text(Msg::MlMarkInHover), edit.unwrap_or(""));
                    ui.label(caption(l.text(Msg::MediaTrimOut)));
                    let out_response = ui
                        .add_enabled(edit.is_none(), DragValue::new(&mut mark_out).speed(1.0))
                        .hint(l.text(Msg::MlMarkOutHover), edit.unwrap_or(""));
                    for (response, id, value) in [
                        (&in_response, in_id, mark_in),
                        (&out_response, out_id, mark_out),
                    ] {
                        if response.dragged() || response.has_focus() {
                            ui.data_mut(|data| data.insert_temp(id, value));
                        } else {
                            ui.data_mut(|data| data.remove::<u64>(id));
                        }
                    }
                    let committed = |response: &egui::Response| {
                        response.drag_stopped()
                            || response.lost_focus()
                            || (response.changed() && !response.dragged() && !response.has_focus())
                    };
                    let mark_out = edited_mark_out(mark_in, mark_out);
                    if (committed(&in_response) || committed(&out_response))
                        && (mark_in, mark_out) != (item.mark_in, item.mark_out)
                    {
                        let mut items = list.items.clone();
                        items[index].mark_in = mark_in;
                        items[index].mark_out = mark_out;
                        operation = Some(set(items, options_of(list)));
                    }
                });
            });
        // Dropping a dragged row here moves it to this row's place.
        if edit.is_none()
            && let Some(from) = row.response.dnd_release_payload::<usize>()
            && *from != index
        {
            operation = Some(set(moved(&list.items, *from, index), options_of(list)));
        }
    }

    // Appending files.
    let add_id = Id::new(("studio-media-list-add", input));
    let mut paths = ui
        .data(|data| data.get_temp::<String>(add_id))
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let taken: Vec<u32> = list.items.iter().map(|item| item.id).collect();
        let added = expand_items(&paths, &taken, &read_playlist);
        let problem = edit.or_else(|| {
            added.as_ref().err().copied().or_else(|| {
                added.as_ref().ok().and_then(|added| {
                    (list.items.len() + added.len() > MAX_MEDIA_LIST_ITEMS)
                        .then_some(Msg::MlReasonTooManyItems.en())
                })
            })
        });
        let width = ui.available_width() - 64.0;
        let field = ui
            .add_enabled(
                edit.is_none(),
                egui::TextEdit::singleline(&mut paths)
                    .id(Id::new(("studio-media-list-add-field", input)))
                    .hint_text("/media/clips/next.mov")
                    .desired_width(width.max(80.0)),
            )
            .hint(l.text(Msg::MlAddPathsHover), edit.unwrap_or(""));
        if field.changed() {
            ui.data_mut(|data| data.insert_temp(add_id, paths.clone()));
        }
        if ui
            .add_enabled(problem.is_none(), small_button(l.text(Msg::MlAdd)))
            .hint(l.text(Msg::MlAddHover), problem.unwrap_or(""))
            .clicked()
            && let Ok(added) = added
        {
            let mut items = list.items.clone();
            items.extend(added);
            operation = Some(set(items, options_of(list)));
            ui.data_mut(|data| data.remove::<String>(add_id));
        }
    });

    if let Some(operation) = operation {
        intents.push(StudioIntent::MediaList { input, operation });
    }
}

/// The Add input dialog's media list form: the files and the options.
pub(crate) fn draw_form(ui: &mut Ui, l: Locale, draft: &mut MediaListDraft) {
    ui.label(section_label(l.text(Msg::MlFiles)));
    ui.add(
        egui::TextEdit::multiline(&mut draft.text)
            .id(Id::new("studio-add-input-media-list-files"))
            .font(egui::TextStyle::Monospace)
            .hint_text("/media/clips/opener.mov\n/media/walk-in.m3u")
            .desired_rows(5)
            .desired_width(ui.available_width()),
    )
    .on_hover_text(l.text(Msg::MlFilesHover));
    let count = draft.items().map_or(0, |items| items.len());
    ui.label(
        caption(l.format(Msg::MlFilesCount, &[("count", Arg::from(count))]))
            .color(text_dim_color()),
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 4.0);
        ui.checkbox(&mut draft.auto_next, l.text(Msg::MlAutoNext))
            .on_hover_text(l.text(Msg::MlAutoNextHover));
        ui.checkbox(&mut draft.auto_first, l.text(Msg::MlAutoFirst))
            .on_hover_text(l.text(Msg::MlAutoFirstHover));
        ui.checkbox(&mut draft.loop_list, l.text(Msg::MlLoop))
            .on_hover_text(l.text(Msg::MlLoopHover));
        ui.checkbox(&mut draft.shuffle, l.text(Msg::MlShuffle))
            .on_hover_text(l.text(Msg::MlShuffleHover));
    });
}

/// The out mark of an edit: none for zero (play to the file's end), else
/// at least one frame past the in mark. A typed in mark as large as
/// `u64::MAX` saturates rather than overflowing.
fn edited_mark_out(mark_in: u64, mark_out: u64) -> Option<u64> {
    (mark_out > 0).then_some(mark_out.max(mark_in.saturating_add(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &Path) -> Option<String> {
        None
    }

    #[test]
    fn an_out_mark_follows_the_in_mark_without_overflowing() {
        assert_eq!(edited_mark_out(10, 0), None);
        assert_eq!(edited_mark_out(10, 5), Some(11));
        assert_eq!(edited_mark_out(10, 50), Some(50));
        assert_eq!(edited_mark_out(u64::MAX, 7), Some(u64::MAX));
    }

    #[test]
    fn pasted_files_and_playlists_expand_into_numbered_items() {
        let playlist = |path: &Path| {
            (path == Path::new("/shows/walk-in.m3u")).then(|| {
                "#EXTM3U\n#EXTINF:12,Opener\nclips/opener.mov\nnotes.txt\n/abs/bed.wav\n".to_owned()
            })
        };
        let items = expand_items(
            "/media/a.mp4\n\"/shows/walk-in.m3u\"\n\n/media/a.mp4\n",
            &[1, 2, 3, 4, 6],
            &playlist,
        )
        .unwrap();
        let summary: Vec<(u32, &str, Option<&str>)> = items
            .iter()
            .map(|item| (item.id, item.uri.as_str(), item.title.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                (5, "/media/a.mp4", None),
                (7, "/shows/clips/opener.mov", Some("Opener")),
                (8, "/abs/bed.wav", None),
            ]
        );
        // A list whose ids reached the bound still takes new items.
        let ids: Vec<u32> = expand_items(
            "/m/a.mov\n/m/b.mov",
            &[fm_protocol::MAX_MEDIA_LIST_ITEM_ID, 2],
            &none,
        )
        .unwrap()
        .iter()
        .map(|item| item.id)
        .collect();
        assert_eq!(ids, [1, 3]);
        assert_eq!(expand_items("", &[], &none), Err(Msg::MlReasonNoItems.en()));
        assert_eq!(
            expand_items("/media/notes.txt", &[], &none),
            Err(Msg::MlReasonNotMedia.en())
        );
        assert_eq!(
            expand_items("/media/a.mp4\nclips/b.mp4", &[], &none),
            Err(Msg::MlReasonPathInvalid.en())
        );
        assert_eq!(
            expand_items("asset://media/a.mp4\nC:\\clips\\b.mov", &[], &none)
                .map(|items| items.len()),
            Ok(2)
        );
        assert_eq!(
            expand_items("/media/gone.m3u8", &[], &none),
            Err(Msg::MlReasonPlaylistUnreadable.en())
        );
        let many = (0..=MAX_MEDIA_LIST_ITEMS).fold(String::new(), |mut text, index| {
            use core::fmt::Write as _;
            let _ = writeln!(text, "/m/{index}.mp4");
            text
        });
        assert_eq!(
            expand_items(&many, &[], &none),
            Err(Msg::MlReasonTooManyItems.en())
        );
    }

    #[test]
    fn search_reorder_and_shuffle_seeds_are_pure() {
        let items = expand_items("/m/Opener.mov\n/m/bed.wav\n/m/closer.mkv", &[], &none).unwrap();
        assert_eq!(filtered_items(&items, "  OPEN "), [0]);
        assert_eq!(filtered_items(&items, "/m/"), [0, 1, 2]);
        assert_eq!(filtered_items(&items, ""), [0, 1, 2]);
        assert_eq!(filtered_items(&items, "zzz"), Vec::<usize>::new());
        let ids =
            |items: &[MediaListItemSpec]| items.iter().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(ids(&moved(&items, 0, 2)), [2, 3, 1]);
        assert_eq!(ids(&moved(&items, 2, 0)), [3, 1, 2]);
        assert_eq!(ids(&moved(&items, 5, 0)), [1, 2, 3]);
        assert_eq!(shuffle_seed(&items, 0), shuffle_seed(&items, 0));
        assert_ne!(shuffle_seed(&items, 0), shuffle_seed(&items, 1));
        assert_eq!(item_name(&items[1]), "bed.wav");
        let draft = MediaListDraft {
            text: "/m/a.mov".into(),
            shuffle: true,
            ..MediaListDraft::default()
        };
        let items = draft.items().unwrap();
        let options = draft.options(&items);
        assert!(options.auto_next && !options.loop_list);
        assert_eq!(options.shuffle_seed, Some(shuffle_seed(&items, 0)));
    }
}
