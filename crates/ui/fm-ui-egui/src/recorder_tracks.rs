//! View ▸ Recorder tracks (AU-010).
//!
//! Reads the supervised project bundle's authored recorder track maps and
//! lists each slot's layout, tracks, channels, and expected WAV file names.
//! Track maps are project files, so a Studio connected to a daemon elsewhere
//! has no local bundle to read; the window says so instead of guessing.
//!
//! This is a read-only monitor. The map itself is authored offline with
//! `freemix-cli record-track-map-import`.

use std::path::Path;
use std::time::{Duration, Instant};

use egui::{Label, RichText, TextEdit, Ui};

use crate::l10n::{Arg, Msg};
use crate::theme::{Tone, card, muted_color, status_chip};
use crate::{
    RecorderTrackFileState, RecorderTrackLayout, RecorderTrackRecordingState, StudioIntent,
    StudioShell, StudioUiState,
};

const DIALOG_WIDTH: f32 = 640.0;
/// How often the open window asks for live file status.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// The window's only state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RecorderTracksWindow {
    pub(crate) open: bool,
    /// One canonical document draft per slot, loaded when the window opens.
    drafts: [String; 2],
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
                *draft = state.recorder_track_documents[slot]
                    .clone()
                    .unwrap_or_else(default_document);
            }
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
        egui::ScrollArea::vertical()
            .id_salt("studio-recorder-tracks-scroll")
            .max_height(520.0)
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
    let Some(map) = &state.recorder_track_maps[slot] else {
        ui.label(
            RichText::new(l.text(Msg::RecorderTracksNoMap))
                .size(crate::theme::legible(11.0))
                .color(muted_color()),
        );
        return;
    };
    for track in &map.tracks {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&track.name)
                    .size(crate::theme::legible(12.0))
                    .strong(),
            );
            ui.label(
                RichText::new(l.format(
                    Msg::RecorderTracksChannels,
                    &[("count", Arg::Plain(u64::from(track.channels)))],
                ))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
            );
            ui.with_layout(crate::l10n::trailing_layout(l), |ui| {
                ui.label(
                    RichText::new(track.sources.join(", "))
                        .monospace()
                        .size(crate::theme::legible(10.0))
                        .color(muted_color()),
                );
            });
        });
    }
    ui.add_space(2.0);
    let files = match map.layout {
        RecorderTrackLayout::Interleaved => Msg::RecorderTracksFilesInterleaved,
        RecorderTrackLayout::SplitPerTrack => Msg::RecorderTracksFilesSplit,
    };
    ui.label(
        RichText::new(l.text(files))
            .size(crate::theme::legible(10.0))
            .color(muted_color()),
    );
    ui.add_space(4.0);
    if let Some(status) = &state.recorder_track_status[slot] {
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
        } else {
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
    } else {
        ui.label(
            RichText::new(l.text(Msg::RecorderTracksNoFiles))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
        );
    }
    ui.add_space(4.0);
    ui.add(
        TextEdit::multiline(&mut window.drafts[slot])
            .code_editor()
            .desired_rows(6)
            .desired_width(f32::INFINITY),
    )
    .on_hover_text(l.text(Msg::RecorderTracksEditorHover));
    ui.horizontal(|ui| {
        if ui
            .button(l.text(Msg::RecorderTracksSave))
            .on_hover_text(l.text(Msg::RecorderTracksSaveHover))
            .clicked()
        {
            intents.push(StudioIntent::SaveRecorderTrackMap {
                slot: u8::try_from(slot + 1).unwrap_or(u8::MAX),
                document: window.drafts[slot].clone(),
            });
        }
        if ui
            .button(l.text(Msg::RecorderTracksReload))
            .on_hover_text(l.text(Msg::RecorderTracksReloadHover))
            .clicked()
        {
            window.drafts[slot] = state.recorder_track_documents[slot]
                .clone()
                .unwrap_or_else(default_document);
        }
    });
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
        RecorderTrackRecordingState::Recording => Tone::Pending,
        RecorderTrackRecordingState::Stopped => Tone::Ready,
        RecorderTrackRecordingState::Failed => Tone::Fault,
        RecorderTrackRecordingState::Recovered => Tone::Pending,
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

/// A valid starter map for a slot that has none yet: one stereo Master track.
fn default_document() -> String {
    "freemix-record-track-map 1\ninventory master 2\ntrack 50726f6772616d master 0 master 1\n"
        .to_owned()
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
