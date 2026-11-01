//! View ▸ Recorder tracks (AU-010).
//!
//! Reads the supervised project bundle's authored recorder track maps and
//! lists each slot's layout, tracks, channels, and expected WAV file names.
//! Track maps are project files, so a Studio connected to a daemon elsewhere
//! has no local bundle to read; the window says so instead of guessing.
//!
//! This is a read-only monitor. The map itself is authored offline with
//! `freemix-cli record-track-map-import`.

use egui::{RichText, Ui};

use crate::l10n::{Arg, Msg};
use crate::theme::{Tone, card, muted_color, status_chip};
use crate::{RecorderTrackLayout, StudioIntent, StudioShell, StudioUiState};

const DIALOG_WIDTH: f32 = 640.0;

/// The window's only state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RecorderTracksWindow {
    pub(crate) open: bool,
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
        let l = self.locale;
        let close = crate::menu::help_modal(
            context,
            "studio-recorder-tracks",
            l.text(Msg::RecorderTracksTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, intents),
        );
        if close {
            self.recorder_tracks.open = false;
        }
    }
}

fn draw_contents(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
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
                        draw_slot(ui, state, slot);
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

fn draw_slot(ui: &mut Ui, state: &StudioUiState, slot: usize) {
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
