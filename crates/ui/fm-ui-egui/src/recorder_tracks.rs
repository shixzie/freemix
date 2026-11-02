//! View ▸ Recorder tracks (AU-010).
//!
//! Reads the supervised project bundle's authored recorder track maps and
//! lists each slot's layout, tracks, channels, and expected WAV file names.
//! Track maps are project files, so a Studio connected to a daemon elsewhere
//! has no local bundle to read; the window says so instead of guessing.
//!
//! Each slot has a structured editor: choose the file layout, add and remove
//! tracks, name them, and give every track ordered Master or aux bus channels.
//! Save builds the canonical `freemix-record-track-map 1` document from the
//! draft; the runtime validates it with `fm-record` before replacing the
//! stored map, so an invalid draft never reaches the bundle.

use core::fmt::Write as _;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use egui::{Button, ComboBox, DragValue, Label, RichText, TextEdit, Ui};

use crate::gating::Hint;
use crate::l10n::{Arg, Msg};
use crate::theme::{Tone, card, muted_color, status_chip};
use crate::{
    RecorderTrackBus, RecorderTrackFileState, RecorderTrackLayout, RecorderTrackLimitsView,
    RecorderTrackMapView, RecorderTrackRecordingState, RecorderTrackSource, StudioIntent,
    StudioShell, StudioUiState,
};

const DIALOG_WIDTH: f32 = 680.0;
/// How often the open window asks for live file status.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
/// Limits mirrored from `fm-record` so the draft refuses what the runtime
/// would; the runtime validates the generated document again.
const MAX_TRACKS: usize = 32;
const MAX_TRACK_CHANNELS: usize = 16;
const MAX_TOTAL_CHANNELS: usize = 64;
const MAX_TRACK_NAME_BYTES: usize = 64;
const MAX_BUS_CHANNEL: u16 = 63;
/// Mirrored from `fm-record`'s track-limits document.
const MAX_LIMIT_SECONDS: u32 = 86_400;
const MAX_LIMIT_BYTES: u64 = 1_099_511_627_776;

/// One track in an editable map.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackDraft {
    name: String,
    sources: Vec<RecorderTrackSource>,
}

/// One slot's editable map.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MapDraft {
    layout: RecorderTrackLayout,
    tracks: Vec<TrackDraft>,
}

impl Default for MapDraft {
    fn default() -> Self {
        Self::stereo_master()
    }
}

impl MapDraft {
    /// A valid starter map: one stereo Master track.
    fn stereo_master() -> Self {
        Self {
            layout: RecorderTrackLayout::Interleaved,
            tracks: vec![TrackDraft {
                name: "Program".to_owned(),
                sources: vec![
                    RecorderTrackSource {
                        bus: RecorderTrackBus::Master,
                        channel: 0,
                    },
                    RecorderTrackSource {
                        bus: RecorderTrackBus::Master,
                        channel: 1,
                    },
                ],
            }],
        }
    }

    /// The draft for a stored slot map, or the starter map when the slot has
    /// none.
    fn from_view(view: Option<&RecorderTrackMapView>) -> Self {
        view.map_or_else(Self::stereo_master, |view| Self {
            layout: view.layout,
            tracks: view
                .tracks
                .iter()
                .map(|track| TrackDraft {
                    name: track.name.clone(),
                    sources: track.source_refs.clone(),
                })
                .collect(),
        })
    }

    /// Why this draft cannot become a map, if it cannot.
    fn problem(&self) -> Option<Msg> {
        if self.tracks.is_empty() || self.tracks.len() > MAX_TRACKS {
            return Some(Msg::RecorderTracksDraftNoTracks);
        }
        let mut names: Vec<&str> = Vec::with_capacity(self.tracks.len());
        let mut total = 0;
        for track in &self.tracks {
            if track.name.is_empty()
                || track.name.len() > MAX_TRACK_NAME_BYTES
                || track.name.trim() != track.name
                || track.name.chars().any(char::is_control)
            {
                return Some(Msg::RecorderTracksDraftName);
            }
            if names.contains(&track.name.as_str()) {
                return Some(Msg::RecorderTracksDraftDuplicateName);
            }
            names.push(&track.name);
            if track.sources.is_empty() {
                return Some(Msg::RecorderTracksDraftEmptyTrack);
            }
            if track.sources.len() > MAX_TRACK_CHANNELS {
                return Some(Msg::RecorderTracksDraftTooWide);
            }
            for (index, source) in track.sources.iter().enumerate() {
                if track.sources[..index].contains(source) {
                    return Some(Msg::RecorderTracksDraftDuplicateChannel);
                }
            }
            total += track.sources.len();
        }
        if total > MAX_TOTAL_CHANNELS {
            return Some(Msg::RecorderTracksDraftTooManyChannels);
        }
        None
    }

    /// The canonical `freemix-record-track-map 1` document for this draft.
    ///
    /// Every bus the draft uses is in the inventory with exactly the channels
    /// its tracks reach, so the document is as small as the map allows.
    fn to_document(&self) -> Option<String> {
        if self.problem().is_some() {
            return None;
        }
        let mut inventory: BTreeMap<RecorderTrackBus, u16> = BTreeMap::new();
        for track in &self.tracks {
            for source in &track.sources {
                let entry = inventory.entry(source.bus).or_insert(0);
                *entry = (*entry).max(source.channel.saturating_add(1));
            }
        }
        let mut document = String::from("freemix-record-track-map 1\n");
        if self.layout == RecorderTrackLayout::SplitPerTrack {
            document.push_str("layout split\n");
        }
        for (bus, channels) in &inventory {
            let _ = writeln!(document, "inventory {} {channels}", bus_token(*bus));
        }
        for track in &self.tracks {
            document.push_str("track ");
            document.push_str(&hex(track.name.as_bytes()));
            for source in &track.sources {
                let _ = write!(document, " {} {}", bus_token(source.bus), source.channel);
            }
            document.push('\n');
        }
        Some(document)
    }
}

/// The document bus token: `master` or `aux:<1..=7>`.
fn bus_token(bus: RecorderTrackBus) -> String {
    match bus {
        RecorderTrackBus::Master => "master".to_owned(),
        RecorderTrackBus::Aux(number) => format!("aux:{number}"),
    }
}

/// The console label for a bus, such as `Aux A`.
fn bus_label(bus: RecorderTrackBus) -> String {
    match bus {
        RecorderTrackBus::Master => "Master".to_owned(),
        RecorderTrackBus::Aux(number) => {
            let letter = char::from(b'A' + number.clamp(1, 7) - 1);
            format!("Aux {letter}")
        }
    }
}

/// Lowercase hex, the document's encoding for names.
fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// A free source channel after the track's last one, falling back to Master.
fn next_source(sources: &[RecorderTrackSource]) -> RecorderTrackSource {
    let bus = sources
        .last()
        .map_or(RecorderTrackBus::Master, |source| source.bus);
    let start = sources
        .last()
        .filter(|source| source.bus == bus)
        .map_or(0, |source| source.channel.saturating_add(1));
    for channel in start..=MAX_BUS_CHANNEL {
        let candidate = RecorderTrackSource { bus, channel };
        if !sources.contains(&candidate) {
            return candidate;
        }
    }
    RecorderTrackSource {
        bus: RecorderTrackBus::Master,
        channel: 0,
    }
}

/// A free track name: `Track N` with the first unused `N`.
fn next_track_name(tracks: &[TrackDraft]) -> String {
    for index in 1..=MAX_TRACKS {
        let name = format!("Track {index}");
        if !tracks.iter().any(|track| track.name == name) {
            return name;
        }
    }
    "Track".to_owned()
}

/// The project's track-file rollover limits as an editable draft.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LimitsDraft {
    seconds_on: bool,
    seconds: u32,
    bytes_on: bool,
    bytes: u64,
}

impl Default for LimitsDraft {
    fn default() -> Self {
        Self {
            seconds_on: false,
            seconds: 600,
            bytes_on: false,
            bytes: 1 << 30,
        }
    }
}

impl LimitsDraft {
    fn from_view(view: RecorderTrackLimitsView) -> Self {
        let mut draft = Self::default();
        if let Some(seconds) = view.max_seconds {
            draft.seconds_on = true;
            draft.seconds = seconds;
        }
        if let Some(bytes) = view.max_bytes {
            draft.bytes_on = true;
            draft.bytes = bytes;
        }
        draft
    }

    /// The authored pair; an unchecked limit is off.
    const fn values(self) -> (Option<u32>, Option<u64>) {
        (
            if self.seconds_on {
                Some(self.seconds)
            } else {
                None
            },
            if self.bytes_on {
                Some(self.bytes)
            } else {
                None
            },
        )
    }
}

/// The window's own state: whether it is open, one structured draft per
/// slot, the project's rollover limits, and when live status was last asked
/// for.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RecorderTracksWindow {
    pub(crate) open: bool,
    /// One structured map draft per slot, seeded when the window opens.
    drafts: [MapDraft; 2],
    /// The project's rollover limits, seeded when the window opens.
    limits: LimitsDraft,
    loaded: bool,
    last_refresh: Option<Instant>,
}

impl StudioShell {
    /// Draws the Recorder tracks window while it is open.
    pub(crate) fn draw_recorder_tracks_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if !self.recorder_tracks.open {
            return;
        }
        if state.recorder_tracks_local
            && self
                .recorder_tracks
                .last_refresh
                .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL)
        {
            intents.push(StudioIntent::RefreshRecorderTracks);
            self.recorder_tracks.last_refresh = Some(Instant::now());
            context.request_repaint_after(REFRESH_INTERVAL);
        }
        if !self.recorder_tracks.loaded {
            for (slot, draft) in self.recorder_tracks.drafts.iter_mut().enumerate() {
                *draft = MapDraft::from_view(state.recorder_track_maps[slot].as_ref());
            }
            self.recorder_tracks.limits = LimitsDraft::from_view(state.recorder_track_limits);
            self.recorder_tracks.loaded = true;
        }
        let l = self.locale;
        let close = crate::menu::help_modal(
            context,
            "studio-recorder-tracks",
            l.text(Msg::RecorderTracksTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, &mut self.recorder_tracks, intents),
        );
        if close {
            self.recorder_tracks.open = false;
            self.recorder_tracks.loaded = false;
            self.recorder_tracks.last_refresh = None;
        }
    }
}

fn draw_contents(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut RecorderTracksWindow,
    intents: &mut Vec<StudioIntent>,
) {
    let l = crate::l10n::locale(ui.ctx());
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(
        RichText::new(l.text(Msg::RecorderTracksIntro))
            .size(crate::theme::legible(11.0))
            .color(muted_color()),
    );
    ui.add_space(6.0);
    if let Some(error) = &state.recorder_track_error {
        ui.label(
            RichText::new(error)
                .size(crate::theme::legible(11.0))
                .color(Tone::Fault.color()),
        );
        ui.add_space(4.0);
    }
    if state.recorder_tracks_local {
        draw_limits(ui, window, intents);
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt("studio-recorder-tracks-scroll")
            .max_height(560.0)
            .show(ui, |ui| {
                for slot in 0..state.recorder_track_maps.len() {
                    card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        draw_slot(ui, state, slot, window, intents);
                    });
                    ui.add_space(4.0);
                }
            });
    } else {
        ui.label(
            RichText::new(l.text(Msg::RecorderTracksNoBundle))
                .size(crate::theme::legible(11.0))
                .color(muted_color()),
        );
    }
    ui.add_space(4.0);
    if ui
        .button(l.text(Msg::RecorderTracksRefresh))
        .on_hover_text(l.text(Msg::RecorderTracksRefreshHover))
        .clicked()
    {
        intents.push(StudioIntent::RefreshRecorderTracks);
    }
}

/// The project-wide track-file rollover limits editor.
fn draw_limits(ui: &mut Ui, window: &mut RecorderTracksWindow, intents: &mut Vec<StudioIntent>) {
    let l = crate::l10n::locale(ui.ctx());
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new(l.text(Msg::RecorderTracksLimits))
                .size(crate::theme::legible(12.0))
                .strong(),
        );
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut window.limits.seconds_on,
                l.text(Msg::RecorderTracksLimitTime),
            )
            .on_hover_text(l.text(Msg::RecorderTracksLimitTimeHover));
            let action = l.text(Msg::RecorderTracksLimitSecondsHover);
            let reason = l.text(Msg::RecorderTracksLimitOff);
            ui.add_enabled(
                window.limits.seconds_on,
                DragValue::new(&mut window.limits.seconds)
                    .range(1..=MAX_LIMIT_SECONDS)
                    .speed(1.0)
                    .suffix(" s"),
            )
            .hint(action, if window.limits.seconds_on { "" } else { reason });
        });
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut window.limits.bytes_on,
                l.text(Msg::RecorderTracksLimitSize),
            )
            .on_hover_text(l.text(Msg::RecorderTracksLimitSizeHover));
            let action = l.text(Msg::RecorderTracksLimitBytesHover);
            let reason = l.text(Msg::RecorderTracksLimitOff);
            ui.add_enabled(
                window.limits.bytes_on,
                DragValue::new(&mut window.limits.bytes)
                    .range(1..=MAX_LIMIT_BYTES)
                    .speed(1_048_576.0)
                    .suffix(" B"),
            )
            .hint(action, if window.limits.bytes_on { "" } else { reason });
        });
        let (max_seconds, max_bytes) = window.limits.values();
        if ui
            .button(l.text(Msg::RecorderTracksSaveLimits))
            .on_hover_text(l.text(Msg::RecorderTracksSaveLimitsHover))
            .clicked()
        {
            intents.push(StudioIntent::SaveRecorderTrackLimits {
                max_seconds,
                max_bytes,
            });
        }
    });
}

fn draw_slot(
    ui: &mut Ui,
    state: &StudioUiState,
    slot: usize,
    window: &mut RecorderTracksWindow,
    intents: &mut Vec<StudioIntent>,
) {
    let l = crate::l10n::locale(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(l.format(
                Msg::RecorderTracksSlot,
                &[("slot", Arg::Plain(u64::try_from(slot + 1).unwrap_or(1)))],
            ))
            .size(crate::theme::legible(13.0))
            .strong(),
        );
        if let Some(map) = &state.recorder_track_maps[slot] {
            status_chip(
                ui,
                l.text(layout_label(map.layout)),
                Tone::Idle,
                chip_width(l.text(layout_label(map.layout))),
            );
            ui.label(
                RichText::new(l.format(
                    Msg::RecorderTracksChannels,
                    &[("count", Arg::Plain(u64::from(map.channels)))],
                ))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
            );
        }
    });
    draw_live_status(ui, state, slot);
    ui.add_space(4.0);
    draw_editor(ui, state, window, slot, intents);
    if window.drafts[slot].problem().is_none() {
        ui.add_space(2.0);
        let files = match window.drafts[slot].layout {
            RecorderTrackLayout::Interleaved => Msg::RecorderTracksFilesInterleaved,
            RecorderTrackLayout::SplitPerTrack => Msg::RecorderTracksFilesSplit,
        };
        ui.label(
            RichText::new(l.text(files))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
        );
    }
}

/// The daemon's live track-file rows, when it has reported any.
fn draw_live_status(ui: &mut Ui, state: &StudioUiState, slot: usize) {
    let l = crate::l10n::locale(ui.ctx());
    let Some(status) = &state.recorder_track_status[slot] else {
        ui.label(
            RichText::new(l.text(Msg::RecorderTracksNoFiles))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
        );
        return;
    };
    status_chip(
        ui,
        l.text(recording_label(status.state)),
        recording_tone(status.state),
        chip_width(l.text(recording_label(status.state))),
    );
    if status.files.is_empty() {
        ui.label(
            RichText::new(l.text(Msg::RecorderTracksNoFiles))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
        );
        return;
    }
    for file in status.files.iter().take(6) {
        let name = Path::new(&file.path)
            .file_name()
            .map_or_else(|| file.path.as_str(), |name| name.to_str().unwrap_or(""));
        ui.horizontal(|ui| {
            ui.add(
                Label::new(
                    RichText::new(name)
                        .monospace()
                        .size(crate::theme::legible(10.0)),
                )
                .truncate(),
            )
            .on_hover_text(&file.path);
            ui.label(
                RichText::new(l.format(
                    Msg::RecorderTracksFileBytes,
                    &[("count", Arg::Count(file.bytes))],
                ))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
            );
            ui.label(
                RichText::new(l.text(file_label(file.state)))
                    .size(crate::theme::legible(10.0))
                    .color(muted_color()),
            );
        });
    }
    if status.files.len() > 6 {
        ui.label(
            RichText::new(l.format(
                Msg::RecorderTracksMoreFiles,
                &[(
                    "count",
                    Arg::Count(u64::try_from(status.files.len() - 6).unwrap_or(u64::MAX)),
                )],
            ))
            .size(crate::theme::legible(10.0))
            .color(muted_color()),
        );
    }
}

/// The structured map editor for one slot.
#[allow(clippy::too_many_lines)] // one pass down a slot's tracks and channels
fn draw_editor(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut RecorderTracksWindow,
    slot: usize,
    intents: &mut Vec<StudioIntent>,
) {
    let l = crate::l10n::locale(ui.ctx());
    let mut remove_track = None;
    let mut add_track = false;
    {
        let draft = &mut window.drafts[slot];
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(l.text(Msg::RecorderTracksLayout))
                    .size(crate::theme::legible(11.0))
                    .strong(),
            );
            ComboBox::from_id_salt(("recorder-tracks-layout", slot))
                .selected_text(l.text(layout_label(draft.layout)))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut draft.layout,
                        RecorderTrackLayout::Interleaved,
                        l.text(Msg::RecorderTracksLayoutInterleaved),
                    )
                    .on_hover_text(l.text(Msg::RecorderTracksLayoutInterleavedHover));
                    ui.selectable_value(
                        &mut draft.layout,
                        RecorderTrackLayout::SplitPerTrack,
                        l.text(Msg::RecorderTracksLayoutSplit),
                    )
                    .on_hover_text(l.text(Msg::RecorderTracksLayoutSplitHover));
                })
                .response
                .on_hover_text(l.text(Msg::RecorderTracksLayoutHover));
            let can_add = draft.tracks.len() < MAX_TRACKS;
            let add = ui
                .add_enabled(can_add, Button::new(l.text(Msg::RecorderTracksAddTrack)))
                .hint(
                    l.text(Msg::RecorderTracksAddTrackHover),
                    if can_add {
                        ""
                    } else {
                        l.text(Msg::RecorderTracksDraftTooManyTracks)
                    },
                );
            if add.clicked() {
                add_track = true;
            }
        });
        for (track_index, track) in draft.tracks.iter_mut().enumerate() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut track.name)
                        .desired_width(140.0)
                        .hint_text(l.text(Msg::RecorderTracksTrackName)),
                )
                .on_hover_text(l.text(Msg::RecorderTracksTrackNameHover));
                ui.label(
                    RichText::new(l.format(
                        Msg::RecorderTracksChannels,
                        &[(
                            "count",
                            Arg::Plain(u64::try_from(track.sources.len()).unwrap_or(u64::MAX)),
                        )],
                    ))
                    .size(crate::theme::legible(10.0))
                    .color(muted_color()),
                );
                ui.with_layout(crate::l10n::trailing_layout(l), |ui| {
                    if ui
                        .button(l.text(Msg::RecorderTracksRemoveTrack))
                        .on_hover_text(l.text(Msg::RecorderTracksRemoveTrackHover))
                        .clicked()
                    {
                        remove_track = Some(track_index);
                    }
                });
            });
            let mut remove_source = None;
            let mut add_source = false;
            for (source_index, source) in track.sources.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.push_id(
                        ("recorder-tracks-source", slot, track_index, source_index),
                        |ui| {
                            ComboBox::from_id_salt((
                                "recorder-tracks-bus",
                                slot,
                                track_index,
                                source_index,
                            ))
                            .selected_text(bus_label(source.bus))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut source.bus,
                                    RecorderTrackBus::Master,
                                    "Master",
                                )
                                .on_hover_text(l.text(Msg::RecorderTracksBusEntryHover));
                                for number in 1..=7 {
                                    ui.selectable_value(
                                        &mut source.bus,
                                        RecorderTrackBus::Aux(number),
                                        bus_label(RecorderTrackBus::Aux(number)),
                                    )
                                    .on_hover_text(l.text(Msg::RecorderTracksBusEntryHover));
                                }
                            })
                            .response
                            .on_hover_text(l.text(Msg::RecorderTracksBusHover));
                            ui.add(
                                DragValue::new(&mut source.channel)
                                    .range(0..=MAX_BUS_CHANNEL)
                                    .speed(0.2),
                            )
                            .on_hover_text(l.text(Msg::RecorderTracksChannelHover));
                            if ui
                                .button(l.text(Msg::RecorderTracksRemoveChannel))
                                .on_hover_text(l.text(Msg::RecorderTracksRemoveChannelHover))
                                .clicked()
                            {
                                remove_source = Some(source_index);
                            }
                        },
                    );
                });
            }
            if let Some(index) = remove_source {
                track.sources.remove(index);
            }
            if ui
                .button(l.text(Msg::RecorderTracksAddChannel))
                .on_hover_text(l.text(Msg::RecorderTracksAddChannelHover))
                .clicked()
            {
                add_source = true;
            }
            if add_source {
                let source = next_source(&track.sources);
                track.sources.push(source);
            }
        }
        if add_track {
            let name = next_track_name(&draft.tracks);
            draft.tracks.push(TrackDraft {
                name,
                sources: vec![RecorderTrackSource {
                    bus: RecorderTrackBus::Master,
                    channel: 0,
                }],
            });
        }
    }
    if let Some(index) = remove_track {
        window.drafts[slot].tracks.remove(index);
    }
    ui.add_space(4.0);
    let problem = window.drafts[slot].problem();
    let problem_text = problem.map(|message| l.text(message));
    ui.horizontal(|ui| {
        let valid = problem.is_none();
        let save = ui
            .add_enabled(valid, Button::new(l.text(Msg::RecorderTracksSave)))
            .hint(
                l.text(Msg::RecorderTracksSaveHover),
                problem_text.unwrap_or(""),
            );
        if save.clicked()
            && let Some(document) = window.drafts[slot].to_document()
        {
            intents.push(StudioIntent::SaveRecorderTrackMap {
                slot: u8::try_from(slot + 1).unwrap_or(u8::MAX),
                document,
            });
        }
        if ui
            .button(l.text(Msg::RecorderTracksReload))
            .on_hover_text(l.text(Msg::RecorderTracksReloadHover))
            .clicked()
        {
            window.drafts[slot] = MapDraft::from_view(state.recorder_track_maps[slot].as_ref());
        }
    });
    if let Some(message) = problem {
        ui.label(
            RichText::new(l.text(message))
                .size(crate::theme::legible(10.0))
                .color(Tone::Fault.color()),
        );
    }
}

const fn recording_label(state: RecorderTrackRecordingState) -> Msg {
    match state {
        RecorderTrackRecordingState::Recording => Msg::RecorderTracksStateRecording,
        RecorderTrackRecordingState::Stopped => Msg::RecorderTracksStateStopped,
        RecorderTrackRecordingState::Failed => Msg::RecorderTracksStateFailed,
        RecorderTrackRecordingState::Recovered => Msg::RecorderTracksStateRecovered,
    }
}

const fn recording_tone(state: RecorderTrackRecordingState) -> Tone {
    match state {
        RecorderTrackRecordingState::Recording | RecorderTrackRecordingState::Recovered => {
            Tone::Pending
        }
        RecorderTrackRecordingState::Stopped => Tone::Ready,
        RecorderTrackRecordingState::Failed => Tone::Fault,
    }
}

const fn file_label(state: RecorderTrackFileState) -> Msg {
    match state {
        RecorderTrackFileState::Open => Msg::RecorderTracksFileOpen,
        RecorderTrackFileState::Closed => Msg::RecorderTracksFileClosed,
        RecorderTrackFileState::Failed => Msg::RecorderTracksFileFailed,
        RecorderTrackFileState::Recovered => Msg::RecorderTracksFileRecovered,
        RecorderTrackFileState::Lost => Msg::RecorderTracksFileLost,
    }
}

const fn layout_label(layout: RecorderTrackLayout) -> Msg {
    match layout {
        RecorderTrackLayout::Interleaved => Msg::RecorderTracksLayoutInterleaved,
        RecorderTrackLayout::SplitPerTrack => Msg::RecorderTracksLayoutSplit,
    }
}

/// A status chip wide enough for `text`.
fn chip_width(text: &str) -> f32 {
    let characters = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
    crate::theme::legible(9.0).mul_add(0.62 * f32::from(characters), 14.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(source: RecorderTrackSource) -> String {
        format!("{} {}", bus_label(source.bus), source.channel)
    }

    #[test]
    fn the_starter_map_writes_the_canonical_document() {
        let document = MapDraft::stereo_master().to_document().unwrap();
        assert_eq!(
            document,
            "freemix-record-track-map 1\n\
             inventory master 2\n\
             track 50726f6772616d master 0 master 1\n"
        );
    }

    #[test]
    fn drafts_inventory_only_the_channels_they_reach() {
        let draft = MapDraft {
            layout: RecorderTrackLayout::SplitPerTrack,
            tracks: vec![
                TrackDraft {
                    name: "Program".to_owned(),
                    sources: vec![
                        RecorderTrackSource {
                            bus: RecorderTrackBus::Master,
                            channel: 0,
                        },
                        RecorderTrackSource {
                            bus: RecorderTrackBus::Master,
                            channel: 1,
                        },
                    ],
                },
                TrackDraft {
                    name: "Bus A".to_owned(),
                    sources: vec![RecorderTrackSource {
                        bus: RecorderTrackBus::Aux(1),
                        channel: 2,
                    }],
                },
            ],
        };
        let document = draft.to_document().unwrap();
        assert_eq!(
            document,
            "freemix-record-track-map 1\n\
             layout split\n\
             inventory master 2\n\
             inventory aux:1 3\n\
             track 50726f6772616d master 0 master 1\n\
             track 4275732041 aux:1 2\n"
        );
    }

    #[test]
    fn drafts_refuse_what_the_model_refuses() {
        let mut draft = MapDraft::stereo_master();
        assert!(draft.to_document().is_some());
        draft.tracks[0].name.clear();
        assert_eq!(draft.problem(), Some(Msg::RecorderTracksDraftName));
        draft.tracks[0].name = "Program".to_owned();
        draft.tracks[0].sources.clear();
        assert_eq!(draft.problem(), Some(Msg::RecorderTracksDraftEmptyTrack));
        draft.tracks[0].sources.push(RecorderTrackSource {
            bus: RecorderTrackBus::Master,
            channel: 0,
        });
        draft.tracks[0].sources.push(RecorderTrackSource {
            bus: RecorderTrackBus::Master,
            channel: 0,
        });
        assert_eq!(
            draft.problem(),
            Some(Msg::RecorderTracksDraftDuplicateChannel)
        );
        draft.tracks.clear();
        assert_eq!(draft.problem(), Some(Msg::RecorderTracksDraftNoTracks));
    }

    #[test]
    fn the_next_source_skips_channels_already_used() {
        let sources = [
            RecorderTrackSource {
                bus: RecorderTrackBus::Aux(2),
                channel: 0,
            },
            RecorderTrackSource {
                bus: RecorderTrackBus::Aux(2),
                channel: 1,
            },
            RecorderTrackSource {
                bus: RecorderTrackBus::Aux(2),
                channel: 2,
            },
        ];
        assert_eq!(
            path(next_source(&sources)),
            "Aux B 3",
            "the next channel continues the last bus"
        );
        assert_eq!(path(next_source(&[])), "Master 0");
    }

    #[test]
    fn limit_drafts_survive_a_round_trip() {
        let view = RecorderTrackLimitsView {
            max_seconds: Some(600),
            max_bytes: Some(1 << 30),
        };
        let draft = LimitsDraft::from_view(view);
        assert_eq!(draft.values(), (Some(600), Some(1 << 30)));
        assert!(draft.seconds_on);
        assert!(draft.bytes_on);
        let off = LimitsDraft::from_view(RecorderTrackLimitsView::default());
        assert_eq!(off.values(), (None, None));
        assert!(!off.seconds_on);
        assert!(!off.bytes_on);
    }

    #[test]
    fn drafts_seed_from_a_stored_map() {
        let view = RecorderTrackMapView {
            slot: 1,
            layout: RecorderTrackLayout::SplitPerTrack,
            channels: 3,
            tracks: vec![crate::RecorderTrackView {
                name: "Bus A".to_owned(),
                channels: 1,
                source_refs: vec![RecorderTrackSource {
                    bus: RecorderTrackBus::Aux(1),
                    channel: 2,
                }],
                sources: vec!["aux A 2".to_owned()],
            }],
        };
        let draft = MapDraft::from_view(Some(&view));
        assert_eq!(draft.layout, RecorderTrackLayout::SplitPerTrack);
        assert_eq!(draft.tracks[0].name, "Bus A");
        assert_eq!(draft.tracks[0].sources, view.tracks[0].source_refs);
    }
}
