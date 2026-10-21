//! The Media side tab (IN-003): one transport bar per media-file input.
//!
//! Each bar shows the replicated transport (state, playhead against the
//! playable end, a growing file's live badge and its waiting-for-data
//! indicator) and offers Play, Pause, Stop, a scrubber, and, for a growing
//! file, Finish. Those are operator controls (`can_transition`). The trim
//! handles, the speed menu (0.25× to 4×), the end action, and the audio
//! track are project edits (`can_edit_project`, padlocks honoured), saved
//! with the project.
//!
//! Under the bars, a panel per slideshow input steps its slides (IN-009,
//! see [`crate::still_input_ui`]), an operator control like the transport.

use core::fmt::Write as _;

use egui::{ComboBox, DragValue, Id, RichText, Slider, Ui, Vec2};
use fm_protocol::{
    MediaAudioTrack, MediaEndAction, MediaOperation, MediaSeekTarget, MediaStatus,
    MediaTransportState,
};
use fm_types::{FrameRate, InputId};

use crate::edit_history::input_edit_lock_reason;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::input_name;
use crate::playlist::{key_button, timecode};
use crate::theme::{
    AMBER, PREVIEW, Tone, caption, card, empty_state, mono, status_chip, text_color,
};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

/// The speed menu, in basis points: 0.25× to 4×.
pub const MEDIA_SPEEDS: [u32; 9] = [
    2_500, 5_000, 7_500, 10_000, 12_500, 15_000, 20_000, 30_000, 40_000,
];

/// The end actions in menu order.
const END_ACTIONS: [MediaEndAction; 3] = [
    MediaEndAction::Stop,
    MediaEndAction::Hold,
    MediaEndAction::Loop,
];

/// Formats a speed in basis points as a multiplier (`0.25×`, `1×`, `4×`).
#[must_use]
pub fn speed_label(speed_basis_points: u32) -> String {
    let whole = speed_basis_points / 10_000;
    let fraction = speed_basis_points % 10_000;
    if fraction == 0 {
        format!("{whole}×")
    } else {
        let digits = format!("{fraction:04}");
        format!("{whole}.{}×", digits.trim_end_matches('0'))
    }
}

/// Why one media input's transport keys are unavailable, if they are: the
/// session gates (operator control), then a source that is not open.
#[must_use]
pub fn media_transport_block(state: &StudioUiState, status: &MediaStatus) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_transition;
    if !ready {
        return Some(unavailable_reason(state, Capability::Transition));
    }
    (!status.transport.available).then_some(Msg::MediaReasonUnavailable.en())
}

/// Why one media input's playback settings cannot be edited, if they
/// cannot: the session gates (project edit), then a padlock.
#[must_use]
pub fn media_settings_block(state: &StudioUiState, input: InputId) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_edit_project;
    if !ready {
        return Some(unavailable_reason(state, Capability::EditProject));
    }
    input_edit_lock_reason(state, input)
}

/// The Media tab body.
pub(crate) fn draw_media(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    ui.add(egui::Label::new(caption(l.text(Msg::MediaIntro))).wrap());
    ui.add_space(2.0);
    let (Some(view), Some(project)) = (state.view.as_ref(), state.project_state.as_ref()) else {
        empty_state(ui, l.text(Msg::TabMedia), l.text(Msg::MediaNotSynced));
        return;
    };
    let slideshows = crate::still_input_ui::slideshows(view);
    if project.media().is_empty() && slideshows.is_empty() {
        // Document inputs (IN-014) share the tab.
        if project.paged_statuses().next().is_none() {
            empty_state(ui, l.text(Msg::MediaNone), l.text(Msg::MediaNoneDetail));
        } else {
            crate::documents_ui::draw_documents(ui, state, intents);
        }
        return;
    }
    let rate = project.frame_rate();
    if project
        .media()
        .values()
        .any(|status| status.transport.state.is_advancing())
    {
        ui.ctx()
            .request_repaint_after(core::time::Duration::from_millis(100));
    }
    for (&input, status) in project.media() {
        let name = input_name(l, &view.inputs, &view.input_names, input);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            draw_bar(ui, l, state, (input, &name), status, rate, intents);
            if let Some(list) = project.media_list(input) {
                crate::media_list_ui::draw_list(ui, l, state, input, list, intents);
            }
        });
        ui.add_space(6.0);
    }
    if !slideshows.is_empty() {
        ui.label(crate::theme::section_label(l.text(Msg::MediaSlideshows)));
    }
    let block = slideshow_block(state);
    let now = ui.input(|input| input.time);
    for (input, still) in slideshows {
        let name = input_name(l, &view.inputs, &view.input_names, input);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            crate::still_input_ui::draw_slideshow_panel(
                ui,
                l,
                (input, &name),
                still,
                block,
                intents,
            );
        });
        ui.add_space(6.0);
        // The slide strip fills window by window; a window asked for is
        // asked again only if no reply arrived within two seconds.
        if let Some(offset) = crate::still_input_ui::missing_window(still) {
            let id = Id::new(("studio-slideshow-request", input));
            let key = (still.status.generation, offset);
            let asked = ui
                .ctx()
                .data(|data| data.get_temp::<((u64, u32), f64)>(id))
                .is_some_and(|(asked, at)| asked == key && now - at < 2.0);
            if !asked {
                ui.ctx().data_mut(|data| data.insert_temp(id, (key, now)));
                intents.push(StudioIntent::RequestSlideshowSlides { input, offset });
            }
        }
    }
    crate::documents_ui::draw_documents(ui, state, intents);
}

/// Why slideshow slides cannot be stepped, if they cannot: the session
/// gates of an operator control.
fn slideshow_block(state: &StudioUiState) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_transition;
    (!ready).then(|| unavailable_reason(state, Capability::Transition))
}

fn state_chip(state: MediaTransportState) -> (Msg, Tone) {
    match state {
        MediaTransportState::Stopped => (Msg::Stopped, Tone::Idle),
        MediaTransportState::Playing => (Msg::PlaylistStatePlaying, Tone::OnAir),
        MediaTransportState::Paused => (Msg::PlaylistStatePaused, Tone::Pending),
        MediaTransportState::Waiting => (Msg::MediaStateWaiting, Tone::Pending),
        MediaTransportState::Ended => (Msg::MediaStateEnded, Tone::Ready),
    }
}

/// The localized name of an ISO 639-2 language tag, or the tag itself.
fn language_name(l: Locale, tag: &str) -> String {
    let msg = match tag {
        "eng" | "en" => Msg::MediaLanguageEnglish,
        "spa" | "es" => Msg::MediaLanguageSpanish,
        "fra" | "fre" | "fr" => Msg::MediaLanguageFrench,
        "deu" | "ger" | "de" => Msg::MediaLanguageGerman,
        "ita" | "it" => Msg::MediaLanguageItalian,
        "por" | "pt" => Msg::MediaLanguagePortuguese,
        _ => return tag.to_owned(),
    };
    l.text(msg).to_owned()
}

/// `2 · Spanish · Commentary (2 ch)`.
fn track_label(l: Locale, track: &MediaAudioTrack) -> String {
    let mut label = track.stream_index.to_string();
    if let Some(language) = &track.language {
        label.push_str(" · ");
        label.push_str(&language_name(l, language));
    }
    if let Some(title) = &track.title {
        label.push_str(" · ");
        label.push_str(title);
    }
    let _ = write!(label, " ({} ch)", track.channels);
    label
}

#[allow(clippy::too_many_lines)] // one transport bar
fn draw_bar(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    (input, name): (InputId, &str),
    status: &MediaStatus,
    project_rate: FrameRate,
    intents: &mut Vec<StudioIntent>,
) {
    let transport = &status.transport;
    let settings = &status.settings;
    let rate = transport.source_rate.unwrap_or(project_rate);
    let transport_block = media_transport_block(state, status);
    let settings_block = media_settings_block(state, input);
    let mut push = |operation| intents.push(StudioIntent::Media { input, operation });

    // Name, state, and the live / waiting / offline badges.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(name).strong().color(text_color()));
        let (label, tone) = state_chip(transport.state);
        status_chip(ui, l.text(label), tone, 86.0).on_hover_text(l.text(Msg::MediaStateHover));
        if settings.growing.is_some() && !transport.complete {
            status_chip(ui, l.text(Msg::Live), Tone::OnAir, 48.0)
                .on_hover_text(l.text(Msg::MediaLiveHover));
        }
        if transport.state == MediaTransportState::Waiting {
            status_chip(ui, l.text(Msg::MediaEofWaiting), Tone::Pending, 120.0)
                .on_hover_text(l.text(Msg::MediaEofWaitingHover));
        }
        if let Some(fault) = transport.fault {
            status_chip(ui, l.text(Msg::MediaOffline), Tone::Fault, 86.0).on_hover_text(l.format(
                Msg::MediaOfflineHover,
                &[("fault", Arg::Str(fault.token()))],
            ));
        } else if !transport.available {
            status_chip(ui, l.text(Msg::MediaOffline), Tone::Fault, 86.0).on_hover_text(l.format(
                Msg::MediaOfflineHover,
                &[("fault", Arg::Str(l.tr(Msg::MediaReasonUnavailable.en())))],
            ));
        }
    });
    ui.label(
        mono(format!(
            "{} / {}",
            timecode(transport.position, rate),
            timecode(transport.playable_end, rate)
        ))
        .size(crate::theme::legible(12.0)),
    )
    .on_hover_text(l.format(
        Msg::MediaPositionHover,
        &[("count", Arg::Count(transport.loops))],
    ));

    // Scrubber: the playhead follows the authority except while dragged.
    let last = transport.frames.max(1) - 1;
    let scrub_id = Id::new(("studio-media-scrub", input));
    let mut position = ui
        .data(|data| data.get_temp::<u64>(scrub_id))
        .unwrap_or(transport.position)
        .min(last);
    ui.spacing_mut().slider_width = (ui.available_width() - 8.0).max(60.0);
    let scrub = ui
        .add_enabled(
            transport_block.is_none() && transport.frames > 0,
            Slider::new(&mut position, 0..=last).show_value(false),
        )
        .hint(
            l.text(Msg::MediaScrubHover),
            transport_block.unwrap_or(Msg::MediaReasonUnavailable.en()),
        );
    if scrub.dragged() {
        ui.data_mut(|data| data.insert_temp(scrub_id, position));
    } else {
        ui.data_mut(|data| data.remove::<u64>(scrub_id));
        if scrub.drag_stopped() || scrub.changed() {
            push(MediaOperation::Seek(MediaSeekTarget::Frame(position)));
        }
    }

    // Transport keys.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let growing = settings.growing.is_some() && !transport.complete;
        // A key is lit only while its state holds (PLAY green while the
        // file plays, PAUSE amber while it is paused); STOP stays a neutral
        // key, since red is reserved for on air.
        let playing = transport.state.is_advancing();
        let paused = transport.state == MediaTransportState::Paused;
        let mut keys = vec![
            (
                MediaOperation::Play,
                l.text(Msg::MediaPlay),
                Msg::MediaPlayHover,
                playing.then_some(PREVIEW),
            ),
            (
                MediaOperation::Pause,
                l.text(Msg::PlaylistPause),
                Msg::MediaPauseHover,
                paused.then_some(AMBER),
            ),
            (
                MediaOperation::Stop,
                l.text(Msg::PlaylistStop),
                Msg::MediaStopHover,
                None,
            ),
        ];
        if growing {
            keys.push((
                MediaOperation::CompleteSource,
                l.text(Msg::MediaComplete),
                Msg::MediaCompleteHover,
                None,
            ));
        }
        #[allow(clippy::cast_precision_loss)] // at most four keys
        let width = ((ui.available_width() - 4.0 * (keys.len() - 1) as f32) / keys.len() as f32)
            .floor()
            .max(48.0);
        for (operation, label, hover, tone) in keys {
            let enabled = transport_block.is_none();
            let response = ui
                .add_enabled_ui(enabled, |ui| {
                    ui.add_sized(
                        Vec2::new(width, 24.0),
                        key_button(
                            &label.to_uppercase(),
                            enabled.then_some(tone).flatten(),
                            width,
                        ),
                    )
                })
                .inner
                .hint(l.text(hover), transport_block.unwrap_or(""));
            if response.clicked() {
                push(operation);
            }
        }
    });

    // Trim handles.
    let editable = settings_block.is_none();
    let reason = settings_block.unwrap_or("");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let frames = transport.frames.max(1);
        let trim_out = settings.trim_out.unwrap_or(frames).min(frames);
        let in_id = Id::new(("studio-media-trim-in", input));
        let out_id = Id::new(("studio-media-trim-out", input));
        let mut trim_in = ui
            .data(|data| data.get_temp::<u64>(in_id))
            .unwrap_or(settings.trim_in);
        let mut out = ui
            .data(|data| data.get_temp::<u64>(out_id))
            .unwrap_or(trim_out);
        ui.label(caption(l.text(Msg::MediaTrimIn)));
        let in_response = ui
            .add_enabled(
                editable,
                DragValue::new(&mut trim_in)
                    .range(0..=trim_out.saturating_sub(1))
                    .speed(1.0),
            )
            .hint(l.text(Msg::MediaTrimInHover), reason);
        ui.label(caption(l.text(Msg::MediaTrimOut)));
        let out_response = ui
            .add_enabled(
                editable,
                DragValue::new(&mut out)
                    .range(settings.trim_in.saturating_add(1)..=frames)
                    .speed(1.0),
            )
            .hint(l.text(Msg::MediaTrimOutHover), reason);
        for (response, id, value) in [(&in_response, in_id, trim_in), (&out_response, out_id, out)]
        {
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
        if committed(&in_response) || committed(&out_response) {
            // The frame count itself means "to the file's end".
            let trim_out = (out < transport.frames).then_some(out);
            if (trim_in, trim_out) != (settings.trim_in, settings.trim_out) {
                push(MediaOperation::Trim { trim_in, trim_out });
            }
        }
    });

    // Speed and end action.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let half = ((ui.available_width() - 4.0) / 2.0).floor();
        ui.add_enabled_ui(editable, |ui| {
            ComboBox::from_id_salt(("studio-media-speed", input))
                .width(half)
                .selected_text(mono(speed_label(settings.speed_basis_points)))
                .show_ui(ui, |ui| {
                    for speed in MEDIA_SPEEDS {
                        let label = speed_label(speed);
                        if ui
                            .selectable_label(settings.speed_basis_points == speed, &label)
                            .on_hover_text(
                                l.format(
                                    Msg::MediaSpeedOptionHover,
                                    &[("speed", Arg::Str(&label))],
                                ),
                            )
                            .clicked()
                            && speed != settings.speed_basis_points
                        {
                            push(MediaOperation::Rate {
                                speed_basis_points: speed,
                            });
                        }
                    }
                })
                .response
                .hint(l.text(Msg::MediaSpeedHover), reason);
        });
        ui.add_enabled_ui(editable, |ui| {
            ComboBox::from_id_salt(("studio-media-end", input))
                .width(half)
                .truncate()
                .selected_text(l.text(end_action_label(settings.end_action).0))
                .show_ui(ui, |ui| {
                    for action in END_ACTIONS {
                        let (label, hover) = end_action_label(action);
                        if ui
                            .selectable_label(settings.end_action == action, l.text(label))
                            .on_hover_text(l.text(hover))
                            .clicked()
                            && action != settings.end_action
                        {
                            push(MediaOperation::EndAction(action));
                        }
                    }
                })
                .response
                .hint(l.text(Msg::MediaEndActionHover), reason);
        });
    });

    // Audio track.
    ui.add_enabled_ui(editable, |ui| {
        let selected = transport
            .tracks
            .iter()
            .find(|track| Some(track.stream_index) == settings.audio_stream);
        let selected_text = if transport.tracks.is_empty() {
            l.text(Msg::MediaNoAudio).to_owned()
        } else {
            selected.map_or_else(
                || l.text(Msg::MediaAudioDefault).to_owned(),
                |track| track_label(l, track),
            )
        };
        ComboBox::from_id_salt(("studio-media-audio", input))
            .width(ui.available_width())
            .truncate()
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(
                        settings.audio_stream.is_none(),
                        l.text(Msg::MediaAudioDefault),
                    )
                    .on_hover_text(l.text(Msg::MediaAudioDefaultHover))
                    .clicked()
                    && settings.audio_stream.is_some()
                {
                    push(MediaOperation::AudioTrack(None));
                }
                for track in &transport.tracks {
                    let label = track_label(l, track);
                    if ui
                        .selectable_label(settings.audio_stream == Some(track.stream_index), &label)
                        .on_hover_text(l.format(
                            Msg::MediaAudioTrackOptionHover,
                            &[("track", Arg::Str(&label))],
                        ))
                        .clicked()
                        && settings.audio_stream != Some(track.stream_index)
                    {
                        push(MediaOperation::AudioTrack(Some(track.stream_index)));
                    }
                }
            })
            .response
            .hint(l.text(Msg::MediaAudioTrackHover), reason);
    });
}

const fn end_action_label(action: MediaEndAction) -> (Msg, Msg) {
    match action {
        MediaEndAction::Stop => (Msg::MediaEndStop, Msg::MediaEndStopHover),
        MediaEndAction::Hold => (Msg::MediaEndHold, Msg::MediaEndHoldHover),
        MediaEndAction::Loop => (Msg::PlaylistLoop, Msg::MediaEndLoopHover),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_and_settings_name_their_first_blocking_gate() {
        let input = InputId::new(core::num::NonZeroU128::new(5).unwrap());
        let status = MediaStatus {
            input: fm_protocol::WireInputId::from_domain(input),
            settings: fm_protocol::MediaSettingsStatus::default(),
            transport: fm_protocol::MediaTransportStatus::default(),
        };
        let disconnected = StudioUiState::new(StudioConnectionStatus::Disconnected);
        assert_eq!(
            media_transport_block(&disconnected, &status),
            Some(Msg::ReasonNoConnection.en())
        );
        assert_eq!(
            media_settings_block(&disconnected, input),
            Some(Msg::ReasonNoConnection.en())
        );
        let unsynchronized = StudioUiState::new(StudioConnectionStatus::Ready);
        assert_eq!(
            media_transport_block(&unsynchronized, &status),
            Some(Msg::ReasonNotSynchronized.en())
        );
    }

    #[test]
    fn speeds_read_as_exact_multipliers() {
        let labels = MEDIA_SPEEDS.map(speed_label);
        assert_eq!(
            labels,
            [
                "0.25×", "0.5×", "0.75×", "1×", "1.25×", "1.5×", "2×", "3×", "4×"
            ]
        );
    }

    #[test]
    fn track_labels_name_the_language_in_the_console_language() {
        let track = MediaAudioTrack {
            stream_index: 2,
            sample_rate: 48_000,
            channels: 2,
            language: Some("spa".into()),
            title: Some("Comentario".into()),
            default: false,
        };
        assert_eq!(
            track_label(Locale::ENGLISH, &track),
            "2 · Spanish · Comentario (2 ch)"
        );
        assert_eq!(
            track_label(Locale::new(fm_ui_model::i18n::Language::Spanish), &track),
            "2 · Español · Comentario (2 ch)"
        );
        let unknown = MediaAudioTrack {
            language: Some("tlh".into()),
            title: None,
            ..track
        };
        assert_eq!(track_label(Locale::ENGLISH, &unknown), "2 · tlh (2 ch)");
    }
}
