//! Preview and Program monitor wells.
//!
//! Each well is a dark 16:9 surface. When the server delivers an encoded
//! fallback frame for the well's tap (Protocol 2.21 `monitor_frame`), the
//! decoded image is letterboxed into the surface with an explicit caption
//! naming its origin and size; otherwise the well shows the routed source
//! name and states that no video feed has arrived. A simulated frame shows
//! the realized source with its scene layers and, on Program, the active
//! overlays; Fade to Black is not composited into it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, ColorImage, CornerRadius, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, TextureId, TextureOptions, Ui, Vec2, pos2,
};
use fm_protocol::{MonitorFrameOrigin, MonitorImage, MonitorTap, WireInputId};
use fm_types::InputId;
use fm_ui_model::{BusSelection, ClientView, SwitcherState};

use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{
    AMBER, MONITOR_BLACK, PREVIEW, PROGRAM, border_color, muted_color, raised_color, routing_color,
    text_color, text_dim_color,
};

/// A delivered frame older than this is shown dimmed and marked STALE.
pub(crate) const MONITOR_STALE_AFTER: Duration = Duration::from_secs(1);

/// Repaint cadence while any monitor shows a frame, so staleness is noticed
/// even when no new state arrives.
pub(crate) const MONITOR_REPAINT_INTERVAL: Duration = Duration::from_millis(250);

/// Height of the tally-colored header strip above each monitor.
pub(crate) const MONITOR_HEADER_HEIGHT: f32 = 26.0;

pub(crate) struct MonitorRouting {
    pub(crate) desired: (InputId, String),
    pub(crate) realized: (InputId, String),
}

pub(crate) fn monitor_routing(
    l: Locale,
    switcher: SwitcherState,
    inputs: &[InputId],
    input_names: &[String],
    select: fn(BusSelection) -> InputId,
) -> MonitorRouting {
    let desired = select(switcher.desired);
    let realized = select(switcher.realized);
    MonitorRouting {
        desired: (desired, input_name(l, inputs, input_names, desired)),
        realized: (realized, input_name(l, inputs, input_names, realized)),
    }
}

pub(crate) fn label_for_input(l: Locale, view: &ClientView, input: InputId) -> String {
    input_name(l, &view.inputs, &view.input_names, input)
}

/// `Input {id}` in the console language, for an input the view does not
/// name.
pub(crate) fn fallback_input_name(l: Locale, input: InputId) -> String {
    l.format(
        Msg::InputFallbackName,
        &[("input", Arg::Str(&input.to_string()))],
    )
}

pub(crate) fn input_name(
    l: Locale,
    inputs: &[InputId],
    input_names: &[String],
    input: InputId,
) -> String {
    inputs
        .iter()
        .position(|candidate| *candidate == input)
        .and_then(|index| input_names.get(index).cloned())
        .unwrap_or_else(|| fallback_input_name(l, input))
}

/// Which bus a monitor well shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Bus {
    Preview,
    Program,
}

impl Bus {
    pub(crate) const fn tap(self) -> MonitorTap {
        match self {
            Self::Preview => MonitorTap::Preview,
            Self::Program => MonitorTap::Program,
        }
    }

    /// The well's header label in the console language.
    fn label(self, l: Locale) -> &'static str {
        l.text(match self {
            Self::Preview => Msg::TallyPreview,
            Self::Program => Msg::TallyProgram,
        })
    }

    /// The bus named inside a sentence (`preview`, `program`).
    fn lowercase_label(self, l: Locale) -> &'static str {
        l.text(match self {
            Self::Preview => Msg::BusPreviewLower,
            Self::Program => Msg::BusProgramLower,
        })
    }

    const fn color(self) -> Color32 {
        match self {
            Self::Preview => PREVIEW,
            Self::Program => PROGRAM,
        }
    }

    fn routing(self, l: Locale, view: &ClientView) -> MonitorRouting {
        monitor_routing(
            l,
            view.switcher,
            &view.inputs,
            &view.input_names,
            match self {
                Self::Preview => |selection: BusSelection| selection.preview,
                Self::Program => |selection: BusSelection| selection.program,
            },
        )
    }
}

/// Facts about one delivered monitor frame that drive its caption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameFacts {
    pub(crate) origin: MonitorFrameOrigin,
    pub(crate) input: WireInputId,
    pub(crate) width: u16,
    pub(crate) height: u16,
    /// Time since the client finished assembling the frame.
    pub(crate) age: Duration,
}

impl FrameFacts {
    pub(crate) fn of(image: &MonitorImage, received_at: Instant, now: Instant) -> Self {
        Self {
            origin: image.origin,
            input: image.input,
            width: image.width,
            height: image.height,
            age: now.saturating_duration_since(received_at),
        }
    }
}

/// An uploaded monitor frame ready to paint.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MonitorPicture {
    pub(crate) texture: TextureId,
    pub(crate) facts: FrameFacts,
}

/// How a well paints its delivered frame, if any.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PictureMode {
    /// No frame: routing placeholder only.
    Hidden,
    /// A current frame, painted at full brightness.
    Live,
    /// A frame that is too old or no longer matches routing, painted dimmed.
    Stale,
}

/// Operator-facing caption, badges, and tooltip note for one well.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MonitorCaption {
    pub(crate) picture: PictureMode,
    /// Badge words as catalog English; worded in the console language when
    /// painted.
    pub(crate) badges: Vec<&'static str>,
    pub(crate) text: String,
    pub(crate) note: String,
}

/// Largest rectangle with the image's aspect ratio centered inside `video`.
pub(crate) fn letterbox(video: Rect, width: u16, height: u16) -> Rect {
    if width == 0 || height == 0 || video.width() <= 0.0 || video.height() <= 0.0 {
        return video;
    }
    let (width, height) = (f32::from(width), f32::from(height));
    let scale = (video.width() / width).min(video.height() / height);
    Rect::from_center_size(video.center(), Vec2::new(width * scale, height * scale))
}

/// Fill of the well outside the canvas (pillar- or letterbox bars), a shade
/// apart from the canvas black so the canvas edge stays visible.
const PILLAR_BLACK: Color32 = Color32::from_rgb(13, 14, 17);

/// Canvas shape assumed until the first monitor frame arrives.
pub(crate) const DEFAULT_CANVAS: (u16, u16) = (16, 9);

/// The canvas shape the monitor wells follow: the latest Program frame's
/// dimensions, else the latest Preview frame's, else the previous shape. The
/// client receives no project video settings, and the encoded fallback frames
/// keep the canvas aspect, so the frames are the shape's only source; keeping
/// the previous shape when frames lapse keeps the layout stable.
pub(crate) fn canvas_shape(
    images: &[Option<(MonitorImage, std::time::Instant)>; 2],
    previous: (u16, u16),
) -> (u16, u16) {
    [MonitorTap::Program, MonitorTap::Preview]
        .into_iter()
        .filter_map(|tap| images[tap.index()].as_ref())
        .map(|(image, _)| (image.width, image.height))
        .find(|(width, height)| *width > 0 && *height > 0)
        .unwrap_or(previous)
}

/// Height of a monitor well `width` points wide for a `canvas` shape.
///
/// A 16:9 canvas fills the width exactly. Taller canvases (4:3, square,
/// portrait) may grow beyond the 16:9 height up to `height_budget`, so a
/// portrait program is pillarboxed in a tall well instead of shrunk into a
/// 16:9 box; wider canvases get a shorter well.
pub(crate) fn monitor_video_height(width: f32, canvas: (u16, u16), height_budget: f32) -> f32 {
    let (canvas_width, canvas_height) = if canvas.0 == 0 || canvas.1 == 0 {
        DEFAULT_CANVAS
    } else {
        canvas
    };
    let natural = width * f32::from(canvas_height) / f32::from(canvas_width);
    let widescreen = width * 9.0 / 16.0;
    natural.min(widescreen.max(height_budget)).max(1.0).round()
}

/// Whether a delivered frame may be shown as current: it must be fresh, and
/// a simulated frame must show the currently realized source. Native frames
/// are read back from the compositor's own output, so they carry the
/// realized source by construction.
pub(crate) fn picture_visible(
    origin: MonitorFrameOrigin,
    frame_input: WireInputId,
    realized_input: Option<WireInputId>,
    age: Duration,
) -> bool {
    age < MONITOR_STALE_AFTER
        && (origin == MonitorFrameOrigin::Native || realized_input == Some(frame_input))
}

/// Caption semantics for one well. `native_runtime` is true when the server
/// is known to deliver native frames (any tap carries a native frame).
pub(crate) fn monitor_caption(
    l: Locale,
    bus: Bus,
    frame: Option<FrameFacts>,
    realized_input: Option<WireInputId>,
    native_runtime: bool,
) -> MonitorCaption {
    let Some(frame) = frame else {
        return if bus == Bus::Preview && native_runtime {
            MonitorCaption {
                picture: PictureMode::Hidden,
                badges: Vec::new(),
                text: l.text(Msg::MonitorNoPreviewTap).to_owned(),
                note: l.text(Msg::MonitorNoPreviewTapNote).to_owned(),
            }
        } else {
            MonitorCaption {
                picture: PictureMode::Hidden,
                badges: Vec::new(),
                text: l.text(Msg::MonitorNoFeed).to_owned(),
                note: l.format(
                    Msg::MonitorNoFeedNote,
                    &[("bus", Arg::Str(bus.lowercase_label(l)))],
                ),
            }
        };
    };
    let simulated = frame.origin == MonitorFrameOrigin::Simulated;
    let (width, height) = (frame.width, frame.height);
    let size = [
        ("width", Arg::Plain(u64::from(width))),
        ("height", Arg::Plain(u64::from(height))),
    ];
    if picture_visible(frame.origin, frame.input, realized_input, frame.age) {
        return if simulated {
            MonitorCaption {
                picture: PictureMode::Live,
                badges: vec![Msg::BadgeSimulated.en()],
                text: l.format(Msg::MonitorSimulated, &size),
                note: l.text(Msg::MonitorSimulatedNote).to_owned(),
            }
        } else {
            MonitorCaption {
                picture: PictureMode::Live,
                badges: Vec::new(),
                text: l.format(Msg::MonitorEncoded, &size),
                note: l.format(Msg::MonitorEncodedNote, &size),
            }
        };
    }
    let text = if frame.age >= MONITOR_STALE_AFTER {
        #[allow(clippy::cast_possible_truncation)] // ages are far below u64::MAX ms
        let age = l.seconds_tenths(frame.age.as_millis() as u64);
        l.format(Msg::MonitorStaleAge, &[("age", Arg::Str(&age))])
    } else {
        l.text(Msg::MonitorStaleMismatch).to_owned()
    };
    MonitorCaption {
        picture: PictureMode::Stale,
        badges: if simulated {
            vec![Msg::BadgeSimulated.en(), Msg::BadgeStale.en()]
        } else {
            vec![Msg::BadgeStale.en()]
        },
        text,
        note: l.text(Msg::MonitorStaleNote).to_owned(),
    }
}

/// Uploaded textures for the latest Preview and Program frames.
///
/// A frame is uploaded once per decoded pixel buffer (`Arc` identity) and the
/// texture is freed as soon as its tap has no frame.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct MonitorTextures {
    slots: [Option<(Arc<[u8]>, TextureHandle)>; 2],
}

impl std::fmt::Debug for MonitorTextures {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_list()
            .entries(
                self.slots
                    .iter()
                    .map(|slot| slot.as_ref().map(|(_, handle)| handle.id())),
            )
            .finish()
    }
}

impl MonitorTextures {
    /// Uploads changed frames and frees textures for taps without a frame.
    pub(crate) fn sync(
        &mut self,
        context: &egui::Context,
        images: &[Option<(MonitorImage, Instant)>; 2],
    ) {
        for tap in MonitorTap::ALL {
            let slot = &mut self.slots[tap.index()];
            let Some((image, _)) = images[tap.index()]
                .as_ref()
                .filter(|(image, _)| image_well_formed(image))
            else {
                *slot = None;
                continue;
            };
            if slot
                .as_ref()
                .is_some_and(|(pixels, _)| Arc::ptr_eq(pixels, &image.rgba))
            {
                continue;
            }
            let color = ColorImage::from_rgba_unmultiplied(
                [usize::from(image.width), usize::from(image.height)],
                &image.rgba,
            );
            if let Some((pixels, handle)) = slot {
                handle.set(color, TextureOptions::LINEAR);
                *pixels = Arc::clone(&image.rgba);
            } else {
                let handle = context.load_texture(
                    format!("fm-monitor-{}", tap.as_str()),
                    color,
                    TextureOptions::LINEAR,
                );
                *slot = Some((Arc::clone(&image.rgba), handle));
            }
        }
    }

    /// The paintable picture for `tap`, if its uploaded texture matches the
    /// state's current frame.
    pub(crate) fn picture(
        &self,
        tap: MonitorTap,
        images: &[Option<(MonitorImage, Instant)>; 2],
        now: Instant,
    ) -> Option<MonitorPicture> {
        let (pixels, handle) = self.slots[tap.index()].as_ref()?;
        let (image, received_at) = images[tap.index()].as_ref()?;
        Arc::ptr_eq(pixels, &image.rgba).then(|| MonitorPicture {
            texture: handle.id(),
            facts: FrameFacts::of(image, *received_at, now),
        })
    }

    #[cfg(test)]
    pub(crate) fn texture_id(&self, tap: MonitorTap) -> Option<TextureId> {
        self.slots[tap.index()]
            .as_ref()
            .map(|(_, handle)| handle.id())
    }
}

fn image_well_formed(image: &MonitorImage) -> bool {
    image.width > 0
        && image.height > 0
        && image.rgba.len() == usize::from(image.width) * usize::from(image.height) * 4
}

/// Size and shape of one monitor well.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WellShape {
    pub(crate) width: f32,
    pub(crate) video_height: f32,
    pub(crate) canvas: (u16, u16),
}

/// Draws one monitor well: its header plus a video area of
/// `shape.video_height`, with the canvas pillar- or letterboxed inside it.
#[allow(clippy::too_many_lines)] // one immediate-mode region
pub(crate) fn draw_monitor(
    ui: &mut Ui,
    bus: Bus,
    view: Option<&ClientView>,
    shape: WellShape,
    picture: Option<MonitorPicture>,
    native_runtime: bool,
) {
    let WellShape {
        width,
        video_height,
        canvas,
    } = shape;
    let routing = view.map(|view| bus.routing(l10n::locale(ui.ctx()), view));
    let fade_to_black = bus == Bus::Program
        && view.is_some_and(|view| view.switcher.desired_fade_to_black.target_active);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, MONITOR_HEADER_HEIGHT + video_height),
        Sense::hover(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }
    let l = l10n::locale(ui.ctx());
    let painter = ui.painter_at(rect.expand(1.0));
    let color = bus.color();
    let header = Rect::from_min_size(rect.min, Vec2::new(width, MONITOR_HEADER_HEIGHT));
    let video = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max);

    painter.rect_filled(
        header,
        CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        },
        raised_color(),
    );
    // Tally bar along the top edge of the header.
    painter.rect_filled(
        Rect::from_min_size(header.min, Vec2::new(width, 3.0)),
        CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        },
        color,
    );
    // The bus name sits on a solid tally chip, legible across the room as
    // on a hardware multiviewer label.
    let label_ink = if bus == Bus::Program {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    let label_galley = painter.layout_no_wrap(
        bus.label(l).to_owned(),
        FontId::new(12.5, FontFamily::Proportional),
        label_ink,
    );
    let chip = Rect::from_min_size(
        pos2(header.min.x + 6.0, header.center().y + 1.5 - 9.0),
        Vec2::new(label_galley.size().x + 14.0, 18.0),
    );
    painter.rect_filled(chip, CornerRadius::same(3), color);
    painter.galley(
        chip.center() - label_galley.size() / 2.0,
        label_galley,
        label_ink,
    );
    let bus_label = chip;
    // Overlay badges and the source name start after the label, whatever
    // the label's length in the console language.
    let label_end = bus_label.max.x.max(header.min.x + 70.0);
    if bus == Bus::Program
        && let Some(view) = view
    {
        // Confirmed active overlay channels keyed over Program (control state).
        let mut x = label_end + 10.0;
        for overlay in view
            .realized_overlays
            .iter()
            .filter(|overlay| overlay.active)
        {
            let badge =
                Rect::from_min_size(pos2(x, header.center().y - 7.0), Vec2::new(26.0, 15.0));
            painter.rect_filled(badge, CornerRadius::same(3), PROGRAM.gamma_multiply(0.25));
            painter.text(
                badge.center(),
                Align2::CENTER_CENTER,
                format!("O{}", overlay.channel),
                FontId::new(10.0, FontFamily::Proportional),
                PROGRAM,
            );
            x += 30.0;
        }
    }
    if let Some(routing) = &routing {
        // Leave room for the bus label and any overlay badges on the left.
        let overlays = if bus == Bus::Program {
            view.map_or(0, |view| {
                view.realized_overlays
                    .iter()
                    .filter(|overlay| overlay.active)
                    .count()
            })
        } else {
            0
        };
        #[allow(clippy::cast_precision_loss)] // at most eight overlay badges
        let reserved = label_end - header.min.x + 20.0 + overlays as f32 * 30.0;
        let galley = crate::theme::elided(
            &painter,
            &routing.realized.1,
            FontId::new(13.5, FontFamily::Proportional),
            text_color(),
            width - reserved - 10.0,
        );
        painter.galley(
            pos2(
                header.max.x - 10.0 - galley.size().x,
                header.center().y + 1.0 - galley.size().y / 2.0,
            ),
            galley,
            text_color(),
        );
    }

    let caption = monitor_caption(
        l,
        bus,
        picture.map(|picture| picture.facts),
        routing
            .as_ref()
            .map(|routing| WireInputId::from_domain(routing.realized.0)),
        native_runtime,
    );
    // The canvas area inside the well; the rest is a darker pillar or letterbox.
    let canvas_area = letterbox(video, canvas.0, canvas.1);
    painter.rect_filled(video, CornerRadius::ZERO, PILLAR_BLACK);
    painter.rect_filled(canvas_area, CornerRadius::ZERO, MONITOR_BLACK);
    if let Some(picture) = picture.filter(|_| caption.picture != PictureMode::Hidden) {
        let tint = if caption.picture == PictureMode::Live {
            Color32::WHITE
        } else {
            Color32::from_gray(70)
        };
        painter.image(
            picture.texture,
            letterbox(canvas_area, picture.facts.width, picture.facts.height),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            tint,
        );
    }
    let live = caption.picture == PictureMode::Live;
    let center = video.center();
    let title_size = (width / 22.0).clamp(15.0, 30.0);
    if !live {
        draw_safe_area(&painter, canvas_area);
        let (title, size, title_color) = match &routing {
            Some(routing) => (routing.realized.1.as_str(), title_size, text_dim_color()),
            None => (
                l.text(Msg::RoutingUnavailable),
                title_size * 0.7,
                muted_color(),
            ),
        };
        let galley = crate::theme::elided(
            &painter,
            title,
            FontId::new(size, FontFamily::Proportional),
            title_color,
            video.width() - 48.0,
        );
        painter.galley(
            center - Vec2::new(0.0, title_size * 0.6) - galley.size() / 2.0,
            galley,
            title_color,
        );
    }
    if let Some(routing) = &routing
        && routing.desired.0 != routing.realized.0
    {
        let galley = crate::theme::elided(
            &painter,
            &l.format(
                Msg::MonitorDesired,
                &[("name", Arg::Str(&routing.desired.1))],
            ),
            FontId::new(12.0, FontFamily::Proportional),
            routing_color(routing.desired.0, routing.realized.0),
            video.width() - 48.0,
        );
        let text_rect = Align2::CENTER_CENTER
            .anchor_size(center + Vec2::new(0.0, title_size * 0.45), galley.size());
        if live {
            painter.rect_filled(
                text_rect.expand2(Vec2::new(6.0, 3.0)),
                CornerRadius::same(3),
                Color32::from_black_alpha(190),
            );
        }
        painter.galley(text_rect.min, galley, Color32::WHITE);
    }
    if live {
        let band = Rect::from_min_max(pos2(video.min.x, video.max.y - 22.0), video.max);
        painter.rect_filled(band, CornerRadius::ZERO, Color32::from_black_alpha(170));
        centered_line(
            &painter,
            band.center(),
            &caption.text,
            FontId::new(10.5, FontFamily::Proportional),
            text_dim_color(),
            video.width() - 16.0,
        );
    } else {
        centered_line(
            &painter,
            pos2(center.x, video.max.y - 30.0),
            &caption.text,
            FontId::new(10.5, FontFamily::Proportional),
            muted_color(),
            video.width() - 16.0,
        );
        centered_line(
            &painter,
            pos2(center.x, video.max.y - 16.0),
            l.text(Msg::MonitorRoutingFootnote),
            FontId::new(9.5, FontFamily::Proportional),
            crate::theme::faded(muted_color(), 0.75),
            video.width() - 16.0,
        );
    }
    let mut badge_right = canvas_area.max.x - 10.0;
    for badge in caption.badges.iter().rev() {
        let galley = painter.layout_no_wrap(
            l.tr(badge).to_owned(),
            FontId::new(10.0, FontFamily::Proportional),
            Color32::BLACK,
        );
        let badge_rect = Rect::from_min_max(
            pos2(
                badge_right - galley.size().x - 12.0,
                canvas_area.min.y + 10.0,
            ),
            pos2(badge_right, canvas_area.min.y + 28.0),
        );
        let fill = if *badge == Msg::BadgeStale.en() {
            muted_color()
        } else {
            AMBER
        };
        badge_backing(&painter, badge_rect);
        painter.rect_filled(badge_rect, CornerRadius::same(3), fill);
        painter.galley(
            Align2::CENTER_CENTER
                .anchor_size(badge_rect.center(), galley.size())
                .min,
            galley,
            Color32::BLACK,
        );
        badge_right = badge_rect.min.x - 6.0;
    }
    if fade_to_black {
        let badge = Rect::from_min_size(
            canvas_area.min + Vec2::new(10.0, 10.0),
            Vec2::new(44.0, 18.0),
        );
        badge_backing(&painter, badge);
        painter.rect_filled(badge, CornerRadius::same(3), PROGRAM);
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            "FTB",
            FontId::new(11.0, FontFamily::Proportional),
            Color32::WHITE,
        );
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, color.gamma_multiply(0.7)),
        StrokeKind::Inside,
    );
    let tooltip = match routing {
        Some(routing) => l.format(
            Msg::MonitorRoutingHover,
            &[
                ("bus", Arg::Str(bus.label(l))),
                ("desired", Arg::Str(&routing.desired.1)),
                ("realized", Arg::Str(&routing.realized.1)),
                ("note", Arg::Str(&caption.note)),
            ],
        ),
        None => caption.note,
    };
    response.on_hover_text(tooltip);
}

/// Height of the audio level strip under a monitor in a tile taller than
/// the picture needs.
pub(crate) const MONITOR_METER_HEIGHT: f32 = 34.0;

/// The dB marks along a monitor's level strip.
const METER_MARKS: [i32; 6] = [-50, -40, -30, -20, -10, 0];

/// A level on the strip's 60 dB scale, `0.0..=1.0`, from a linear
/// amplitude in millionths.
fn level_fraction(millionths: u32) -> f32 {
    if millionths == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)] // a meter reading needs no more
    let amplitude = millionths as f32 / 1_000_000.0;
    ((20.0 * amplitude.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// The meter colour at `fraction` of the 60 dB scale: green to -18 dBFS,
/// amber to -6, red above, as on the mixer's strips.
fn level_color(fraction: f32) -> Color32 {
    if fraction > 54.0 / 60.0 {
        PROGRAM
    } else if fraction > 42.0 / 60.0 {
        AMBER
    } else {
        PREVIEW
    }
}

/// Draws the level strip under a monitor, `width` wide: the bus's audio
/// as horizontal stereo bars (Program: the master mix; Preview: the
/// previewed source's own level) over a dB scale, as a multiviewer shows
/// audio beside each picture. Bars stay empty without a meter feed.
pub(crate) fn draw_monitor_meters(
    ui: &mut Ui,
    bus: Bus,
    width: f32,
    channels: Option<&[fm_protocol::AudioMeterChannel]>,
) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, MONITOR_METER_HEIGHT), Sense::hover());
    let l = l10n::locale(ui.ctx());
    response.on_hover_text(l.text(match bus {
        Bus::Program => Msg::MonitorMetersProgramHover,
        Bus::Preview => Msg::MonitorMetersPreviewHover,
    }));
    if !ui.is_rect_visible(rect) {
        return;
    }
    if channels.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(3), raised_color());
    let font = FontId::new(crate::theme::legible(8.5), FontFamily::Monospace);
    let scale = Rect::from_min_max(
        pos2(rect.min.x + 8.0, rect.min.y + 5.0),
        pos2(rect.max.x - 12.0, rect.min.y + 21.0),
    );
    let bar_height = 7.0;
    for row in 0..2_usize {
        #[allow(clippy::cast_precision_loss)] // two rows
        let top = scale.min.y + row as f32 * (bar_height + 2.0);
        let bar = Rect::from_min_size(pos2(scale.min.x, top), Vec2::new(scale.width(), bar_height));
        painter.rect_filled(bar, CornerRadius::same(1), MONITOR_BLACK);
        let channel = channels.and_then(|channels| {
            channels
                .get(row)
                .or_else(|| (row == 1).then(|| channels.first()).flatten())
        });
        let Some(channel) = channel else {
            continue;
        };
        let rms = level_fraction(channel.rms_millionths);
        // Green, amber, and red zones, each up to the RMS level.
        let mut from = 0.0;
        for to in [42.0 / 60.0, 54.0 / 60.0, 1.0_f32] {
            let end = rms.min(to);
            if end > from {
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(bar.min.x + bar.width() * from, bar.min.y),
                        pos2(bar.min.x + bar.width() * end, bar.max.y),
                    ),
                    CornerRadius::ZERO,
                    level_color(end),
                );
            }
            from = to;
        }
        let peak = level_fraction(channel.peak_millionths);
        if peak > 0.0 {
            let x = bar.min.x + bar.width() * peak;
            painter.line_segment(
                [pos2(x, bar.min.y), pos2(x, bar.max.y)],
                Stroke::new(2.0, level_color(peak)),
            );
        }
    }
    for mark in METER_MARKS {
        #[allow(clippy::cast_precision_loss)] // small dB marks
        let x = scale.min.x + scale.width() * (mark as f32 + 60.0) / 60.0;
        painter.line_segment(
            [pos2(x, scale.max.y - 1.0), pos2(x, scale.max.y + 2.0)],
            Stroke::new(1.0, border_color()),
        );
        painter.text(
            pos2(x, scale.max.y + 3.0),
            Align2::CENTER_TOP,
            mark.to_string(),
            font.clone(),
            text_dim_color(),
        );
    }
}

/// A dark ring behind a badge drawn over the picture, so a badge keeps its
/// edge on any picture: an amber `SIMULATED` badge on an amber source (or a
/// red `FTB` badge on red) still reads as a badge, while on the usual dark
/// or mid-tone picture the ring disappears into it.
fn badge_backing(painter: &egui::Painter, badge: Rect) {
    painter.rect_filled(
        badge.expand(2.0),
        CornerRadius::same(5),
        Color32::from_black_alpha(215),
    );
}

/// One line of text centered on `center`, ending in `…` when it is wider
/// than `max_width` (long translated captions in narrow wells).
fn centered_line(
    painter: &egui::Painter,
    center: egui::Pos2,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) {
    let galley = crate::theme::elided(painter, text, font, color, max_width);
    painter.galley(center - galley.size() / 2.0, galley, color);
}

/// Faint title-safe corner marks so the empty well reads as a monitor.
fn draw_safe_area(painter: &egui::Painter, video: Rect) {
    let guide = Stroke::new(crate::theme::line_width(), border_color());
    let safe = video.shrink2(video.size() * 0.1);
    let arm = (safe.width().min(safe.height()) * 0.08).max(8.0);
    for (corner, dx, dy) in [
        (safe.left_top(), arm, arm),
        (safe.right_top(), -arm, arm),
        (safe.left_bottom(), arm, -arm),
        (safe.right_bottom(), -arm, -arm),
    ] {
        painter.line_segment([corner, corner + Vec2::new(dx, 0.0)], guide);
        painter.line_segment([corner, corner + Vec2::new(0.0, dy)], guide);
    }
}
