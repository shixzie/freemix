//! Right-hand side panel: on-air outputs (recording and streams) above a
//! tabbed area for overlays, stingers, titles, the playlist, and media
//! inputs.

use core::fmt::Write as _;
use core::num::NonZeroU128;

use egui::{
    Align2, Button, Color32, CornerRadius, FontFamily, FontId, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2,
};
use fm_protocol::{StingerReadiness, StreamStatusMessage, StreamStatusSample, WireStingerSlotId};
use fm_ui_model::{StingerStatus, StreamStatus};

use crate::gating::{Capability, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg, frames};
use crate::stinger_config::{self, StingerEditor};
use crate::theme::{
    AMBER, ERROR, PREVIEW, Tone, accent_color, border_color, border_strong_color, caption, card,
    elided, graphite_color, mono, muted_color, paint_lamp, raised_color, section_label, text_color,
    text_dim_color, widget_hover_color,
};
use crate::{
    RecordingAction, StudioConnectionStatus, StudioIntent, StudioShell, StudioUiState,
    TransitionGate, outputs, overlays, transition_availability,
};

/// Tabs of the lower side panel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SideTab {
    #[default]
    Overlays,
    Stingers,
    Titles,
    Playlist,
    Media,
}

impl SideTab {
    /// Every tab in panel order.
    pub(crate) const ALL: [Self; 5] = [
        Self::Overlays,
        Self::Stingers,
        Self::Titles,
        Self::Playlist,
        Self::Media,
    ];

    /// The tab's title.
    pub(crate) const fn title(self) -> Msg {
        match self {
            Self::Overlays => Msg::TabOverlays,
            Self::Stingers => Msg::TabStingers,
            Self::Titles => Msg::TabTitles,
            Self::Playlist => Msg::TabPlaylist,
            Self::Media => Msg::TabMedia,
        }
    }

    /// What the tab holds, for its hover help.
    pub(crate) const fn hover(self) -> Msg {
        match self {
            Self::Overlays => Msg::TabOverlaysHover,
            Self::Stingers => Msg::TabStingersHover,
            Self::Titles => Msg::TabTitlesHover,
            Self::Playlist => Msg::TabPlaylistHover,
            Self::Media => Msg::TabMediaHover,
        }
    }
}

/// The side tabs' label size and letter spacing: the largest step of the
/// ladder at which every title fits `width` (measured, so it holds for
/// every language and theme), else the smallest step.
fn tab_label_style(ui: &Ui, titles: &[String], width: f32, high_contrast: bool) -> (f32, f32) {
    let bump = if high_contrast { 1.0 } else { 0.0 };
    let ladder = [
        (11.0, 1.0),
        (11.0, 0.5),
        (10.5, 0.3),
        (10.0, 0.0),
        (9.5, 0.0),
        (9.0, 0.0),
        (8.5, 0.0),
    ];
    let fits = |(size, letter_spacing): (f32, f32)| {
        titles.iter().all(|title| {
            #[allow(clippy::cast_precision_loss)] // a short tab title
            let spacing = letter_spacing * title.chars().count() as f32;
            let text = ui
                .painter()
                .layout_no_wrap(
                    title.clone(),
                    egui::FontId::proportional(size + bump),
                    text_color(),
                )
                .size()
                .x;
            text + spacing <= width
        })
    };
    let (size, letter_spacing) = ladder
        .into_iter()
        .find(|step| fits(*step))
        .unwrap_or(ladder[ladder.len() - 1]);
    (size + bump, letter_spacing)
}

pub(crate) const STREAMS_NONE_CONFIGURED: &str = Msg::StreamsNoneConfigured.en();

impl StudioShell {
    /// The side panel's tab row: five equal tabs whose labels shrink
    /// together to fit.
    fn draw_side_tabs(&mut self, ui: &mut Ui) {
        let l = self.locale;
        ui.horizontal(|ui| {
            let tabs = SideTab::ALL;
            ui.spacing_mut().item_spacing.x = 2.0;
            let spacing = ui.spacing().item_spacing.x;
            // Every tab gets an exact share of the row, so the row never
            // overflows a narrow panel.
            #[allow(clippy::cast_precision_loss)] // five tabs
            let tab_width = ((ui.available_width() - spacing * (tabs.len() - 1) as f32)
                / tabs.len() as f32)
                .floor()
                .clamp(40.0, 96.0);
            ui.spacing_mut().button_padding.x = 2.0;
            let high_contrast = crate::theme::high_contrast();
            let titles = tabs.map(|tab| l.text(tab.title()).to_uppercase());
            // The labels shrink together (size, then letter spacing) until
            // the longest fits its share, so no tab name is cut at the
            // supported widths; only below the smallest size do they
            // ellipsize, with the name still in the hover help.
            let (size, letter_spacing) = tab_label_style(
                ui,
                &titles,
                tab_width - 2.0 * ui.spacing().button_padding.x - 2.0,
                high_contrast,
            );
            for (tab, title) in tabs.into_iter().zip(titles) {
                let selected = self.side_tab == tab;
                let text = RichText::new(title)
                    .size(size)
                    .strong()
                    .extra_letter_spacing(letter_spacing)
                    .color(if selected {
                        text_color()
                    } else {
                        text_dim_color()
                    });
                let response = ui
                    .add_sized(
                        Vec2::new(tab_width, 26.0),
                        Button::new(text)
                            .fill(if selected {
                                raised_color()
                            } else {
                                Color32::TRANSPARENT
                            })
                            .stroke(Stroke::new(
                                crate::theme::line_width(),
                                if selected {
                                    border_color()
                                } else {
                                    Color32::TRANSPARENT
                                },
                            ))
                            .truncate()
                            .min_size(Vec2::new(tab_width, 26.0)),
                    )
                    .on_hover_text(format!("{}\n{}", l.text(tab.title()), l.text(tab.hover())));
                if selected {
                    let rect = response.rect;
                    ui.painter().line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(2.0, accent_color()),
                    );
                }
                if response.clicked() {
                    self.side_tab = tab;
                }
            }
        });
    }

    pub(crate) fn draw_side_panel(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            if let Some(action) = self.draw_recording(ui, state) {
                intents.push(StudioIntent::Recording(action));
            }
            ui.add_space(6.0);
            outputs::draw_streams(ui, state, intents);
        });
        ui.add_space(8.0);
        self.draw_side_tabs(ui);
        ui.add_space(4.0);
        ScrollArea::vertical()
            .id_salt("studio-side-tabs")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.side_tab {
                SideTab::Overlays => {
                    overlays::draw_overlays(ui, state, self.transition_duration_frames, intents);
                }
                SideTab::Stingers => {
                    draw_stingers(
                        ui,
                        state,
                        &mut self.stinger_editor,
                        self.transition_duration_frames,
                        intents,
                    );
                    stinger_config::draw_stinger_configuration(
                        ui,
                        state,
                        &mut self.stinger_editor,
                        self.transition_duration_frames,
                        intents,
                    );
                }
                SideTab::Titles => {
                    self.titles.draw(ui, state, intents);
                    if let Some(title) = self.titles.take_animate_request() {
                        self.open_animation(Some(title));
                    }
                }
                SideTab::Playlist => self.playlist.draw(ui, state, intents),
                SideTab::Media => crate::media_transport::draw_media(ui, state, intents),
            });
    }

    /// Draws the recorder status rows and returns the selected action.
    ///
    /// Each row exposes exactly one action for the authoritative desired flag
    /// and keeps it disabled while an earlier recording intent is unresolved.
    #[allow(clippy::unused_self)] // the panel reads immutable state; the shell receiver matches `draw_inputs`
    pub fn draw_recording(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
    ) -> Option<RecordingAction> {
        outputs::draw_recorders(ui, state)
    }
}

/// Operator-facing recording status derived only from the replicated desired
/// flag; it is never an optimistic local guess.
pub(crate) const fn recording_status_label(desired: bool) -> &'static str {
    if desired {
        Msg::Recording.en()
    } else {
        Msg::Stopped.en()
    }
}

/// Pure placeholder decision: connected with a view but no configured streams.
pub(crate) const fn streams_show_none_configured(
    connection_status: StudioConnectionStatus,
    has_view: bool,
    streams: &[StreamStatus],
) -> bool {
    matches!(connection_status, StudioConnectionStatus::Ready) && has_view && streams.is_empty()
}

/// Finds the latest lossy status sample for one roster target, if any.
pub(crate) fn latest_stream_sample(
    status: Option<&StreamStatusMessage>,
    target: NonZeroU128,
) -> Option<&StreamStatusSample> {
    status.and_then(|message| {
        message
            .samples
            .iter()
            .find(|sample| sample.target.get() == target)
    })
}

/// The transport diagnostics under a stream row's tooltip, in the console
/// language: link state and counters from the latest sample, any failure
/// code, and local HLS playlist health. Without a sample it says so; the
/// row's own first tooltip line already names the desired and realized
/// state.
pub(crate) fn stream_row_text(l: Locale, sample: Option<&StreamStatusSample>) -> String {
    let Some(sample) = sample else {
        return l.text(Msg::StreamDiagNoSample).to_owned();
    };
    let mut text = l.format(
        Msg::StreamDiagTransport,
        &[
            (
                "link",
                Arg::Str(l.text(if sample.connected {
                    Msg::StreamDiagConnected
                } else {
                    Msg::StreamDiagDisconnected
                })),
            ),
            ("bytes", Arg::Str(&l.bytes(sample.muxed_bytes))),
            ("queued", Arg::Count(sample.enqueued_pairs)),
            ("dropped", Arg::Count(sample.dropped_pairs)),
        ],
    );
    if let Some(failure) = &sample.failure {
        text.push('\n');
        text.push_str(&l.format(Msg::StreamDiagFailure, &[("failure", Arg::Str(failure))]));
    }
    if let Some(local) = sample.local {
        text.push('\n');
        text.push_str(&l.format(
            Msg::StreamDiagLocal,
            &[
                ("count", Arg::from(local.segments)),
                (
                    "target",
                    Arg::Str(&l.seconds_tenths(u64::from(local.target_duration_ms))),
                ),
                ("free", Arg::Str(&l.bytes(local.free_bytes))),
            ],
        ));
    }
    text
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StingerSlotPresentation {
    pub(crate) label: &'static str,
    pub(crate) color: Color32,
    pub(crate) ready: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StingerSlotState {
    Unavailable,
    Unconfigured,
    Readiness(StingerReadiness),
}

pub(crate) fn stinger_slot_presentation(state: StingerSlotState) -> StingerSlotPresentation {
    match state {
        StingerSlotState::Unavailable => StingerSlotPresentation {
            label: Msg::StingerStateUnavailable.en(),
            color: muted_color(),
            ready: false,
        },
        StingerSlotState::Unconfigured => StingerSlotPresentation {
            label: Msg::StingerUnconfigured.en(),
            color: muted_color(),
            ready: false,
        },
        StingerSlotState::Readiness(StingerReadiness::NotRequested) => StingerSlotPresentation {
            label: Msg::StingerNotRequested.en(),
            color: AMBER,
            ready: false,
        },
        StingerSlotState::Readiness(StingerReadiness::Ready) => StingerSlotPresentation {
            label: Msg::StingerReady.en(),
            color: PREVIEW,
            ready: true,
        },
        StingerSlotState::Readiness(StingerReadiness::Missing) => StingerSlotPresentation {
            label: Msg::StingerMissing.en(),
            color: ERROR,
            ready: false,
        },
    }
}

pub(crate) fn stinger_slot_state(stingers: Option<&[StingerStatus]>, slot: u8) -> StingerSlotState {
    let Some(stingers) = stingers else {
        return StingerSlotState::Unavailable;
    };
    stingers
        .iter()
        .find(|status| status.slot == slot)
        .map_or(StingerSlotState::Unconfigured, |status| {
            StingerSlotState::Readiness(status.readiness)
        })
}

/// Lamp tone of one Stinger slot state.
pub(crate) const fn stinger_slot_tone(state: StingerSlotState) -> Tone {
    match state {
        StingerSlotState::Unavailable | StingerSlotState::Unconfigured => Tone::Idle,
        StingerSlotState::Readiness(StingerReadiness::Ready) => Tone::Ready,
        StingerSlotState::Readiness(StingerReadiness::NotRequested) => Tone::Pending,
        StingerSlotState::Readiness(StingerReadiness::Missing) => Tone::Fault,
    }
}

/// What a click on one Stinger slot card does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StingerCardClick {
    /// Fire the slot from Preview to Program.
    Fire,
    /// Select the slot in the Configure section below the grid.
    Configure,
}

/// A ready slot fires when the transition gates allow it; any other click
/// only selects the slot for configuration, which is local and harmless.
pub(crate) const fn stinger_card_click(ready: bool, can_fire: bool) -> StingerCardClick {
    if ready && can_fire {
        StingerCardClick::Fire
    } else {
        StingerCardClick::Configure
    }
}

const STINGER_CARD_HEIGHT: f32 = 50.0;

fn draw_stingers(
    ui: &mut Ui,
    state: &StudioUiState,
    editor: &mut StingerEditor,
    duration_frames: u32,
    intents: &mut Vec<StudioIntent>,
) {
    let can_fire = transition_availability(TransitionGate::from_state(state)).stinger();
    let reason = unavailable_reason(state, Capability::Transition);
    let l = l10n::locale(ui.ctx());
    ui.add(egui::Label::new(caption(l.text(Msg::StingersIntro))).wrap());
    ui.add_space(2.0);
    let stingers = state.view.as_ref().map(|view| view.stingers.as_slice());
    let gap = 6.0;
    let width = ((ui.available_width() - gap) / 2.0).floor().max(80.0);
    for row in [[1_u8, 2], [3, 4], [5, 6], [7, 8]] {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for slot in row {
                let slot_state = stinger_slot_state(stingers, slot);
                let media = state.view.as_ref().and_then(|view| {
                    stinger_config::configured_stinger(view, slot)
                        .map(|status| crate::monitors::label_for_input(l, view, status.media_input))
                });
                let card = StingerCard {
                    slot,
                    state: slot_state,
                    media: media.as_deref(),
                    selected: editor.slot == slot,
                    can_fire,
                    reason,
                    width,
                };
                match draw_stinger_card(ui, &card) {
                    Some(StingerCardClick::Fire) => intents.push(StudioIntent::Stinger {
                        slot: WireStingerSlotId::new(slot)
                            .expect("Studio renders only Stinger slots 1 through 8"),
                        duration_frames,
                    }),
                    Some(StingerCardClick::Configure) if editor.slot != slot => {
                        editor.slot = slot;
                        editor.media = None;
                    }
                    Some(StingerCardClick::Configure) | None => {}
                }
            }
        });
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(section_label(l.text(Msg::Duration)));
        ui.label(
            mono(frames(l, duration_frames))
                .size(crate::theme::legible(11.5))
                .color(text_color()),
        );
        ui.add(egui::Label::new(caption(l.text(Msg::StingerDurationShared))).truncate());
    });
}

struct StingerCard<'a> {
    slot: u8,
    state: StingerSlotState,
    media: Option<&'a str>,
    selected: bool,
    can_fire: bool,
    reason: &'a str,
    width: f32,
}

/// One fixed-size Stinger slot card; returns the click meaning, if clicked.
fn draw_stinger_card(ui: &mut Ui, card: &StingerCard<'_>) -> Option<StingerCardClick> {
    let l = l10n::locale(ui.ctx());
    let presentation = stinger_slot_presentation(card.state);
    let tone = stinger_slot_tone(card.state);
    let click = stinger_card_click(presentation.ready, card.can_fire);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(card.width, STINGER_CARD_HEIGHT), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fires = click == StingerCardClick::Fire;
        let hovered = response.hovered();
        let fill = if fires && response.is_pointer_button_down_on() {
            PREVIEW.gamma_multiply(0.25)
        } else if hovered {
            widget_hover_color()
        } else if fires {
            PREVIEW.gamma_multiply(0.10)
        } else {
            graphite_color()
        };
        painter.rect_filled(rect, CornerRadius::same(4), fill);
        let stroke = if response.has_focus() {
            Stroke::new(crate::theme::focus_ring_width(), accent_color())
        } else if card.selected {
            Stroke::new(1.5, accent_color().gamma_multiply(0.8))
        } else if tone == Tone::Idle {
            Stroke::new(
                crate::theme::line_width(),
                if hovered {
                    border_strong_color()
                } else {
                    border_color()
                },
            )
        } else {
            Stroke::new(
                crate::theme::line_width(),
                tone.color()
                    .gamma_multiply(if hovered { 0.9 } else { 0.55 }),
            )
        };
        painter.rect_stroke(rect, CornerRadius::same(4), stroke, StrokeKind::Inside);
        painter.text(
            pos2(rect.min.x + 9.0, rect.min.y + 13.0),
            Align2::LEFT_CENTER,
            format!("S{}", card.slot),
            FontId::new(13.0, FontFamily::Monospace),
            if tone == Tone::Idle {
                text_dim_color()
            } else {
                text_color()
            },
        );
        let label = elided(
            painter,
            l.tr(presentation.label),
            FontId::new(9.5, FontFamily::Proportional),
            tone.color(),
            rect.width() - 60.0,
        );
        let label_pos = pos2(
            rect.max.x - 9.0 - label.size().x,
            rect.min.y + 13.0 - label.size().y / 2.0,
        );
        paint_lamp(
            painter,
            pos2(label_pos.x - 9.0, rect.min.y + 13.0),
            3.5,
            tone,
        );
        painter.galley(label_pos, label, tone.color());
        let (media, color) = match (card.media, card.state) {
            (Some(media), _) => (media, text_dim_color()),
            (None, StingerSlotState::Unavailable) => {
                (l.text(Msg::StingerStateNotSynced), muted_color())
            }
            (None, _) => (l.text(Msg::StingerEmptySlot), muted_color()),
        };
        let media = elided(
            painter,
            media,
            FontId::new(11.0, FontFamily::Proportional),
            color,
            rect.width() - 18.0,
        );
        painter.galley(pos2(rect.min.x + 9.0, rect.max.y - 20.0), media, color);
    }
    response
        .on_hover_text(stinger_card_tooltip(card, presentation, click, l))
        .clicked()
        .then_some(click)
}

/// The card's tooltip: slot and readiness, media, and what a click does.
fn stinger_card_tooltip(
    card: &StingerCard<'_>,
    presentation: StingerSlotPresentation,
    click: StingerCardClick,
    l: crate::l10n::Locale,
) -> String {
    let mut tooltip = l.format(
        Msg::StingerCardTitle,
        &[
            ("slot", Arg::Plain(u64::from(card.slot))),
            ("state", Arg::Str(l.tr(presentation.label))),
        ],
    );
    if let Some(media) = card.media {
        let _ = write!(
            tooltip,
            "\n{}",
            l.format(Msg::StingerCardMedia, &[("name", Arg::Str(media))])
        );
    }
    match click {
        StingerCardClick::Fire => {
            let _ = write!(tooltip, "\n{}", l.text(Msg::StingerCardFire));
        }
        StingerCardClick::Configure if presentation.ready => {
            let _ = write!(
                tooltip,
                "\n{}\n{}",
                l.format(
                    Msg::StingerCardCannotFire,
                    &[("reason", Arg::Str(l.tr(card.reason)))]
                ),
                l.text(Msg::StingerCardConfigure)
            );
        }
        StingerCardClick::Configure => {
            let _ = write!(
                tooltip,
                "\n{} · {}",
                l.text(Msg::StingerCardNotReady),
                l.text(Msg::StingerCardConfigure)
            );
        }
    }
    tooltip
}
