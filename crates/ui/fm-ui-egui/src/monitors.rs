//! Preview and Program monitor wells.
//!
//! Each well is a dark 16:9 surface. When the server delivers an encoded
//! fallback frame for the well's tap (Protocol 2.21 `monitor_frame`), the
//! decoded image is letterboxed into the surface with an explicit caption
//! naming its origin and size; otherwise the well shows the routed source
//! name and states that no video feed has arrived. A simulated frame shows
//! only the realized source: overlays, scenes, and Fade to Black are not
//! composited into it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, ColorImage, CornerRadius, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, TextureId, TextureOptions, Ui, Vec2, pos2,
};
use fm_protocol::{MonitorFrameOrigin, MonitorImage, MonitorTap, WireInputId};
use fm_types::InputId;
use fm_ui_model::{BusSelection, ClientView, SwitcherState};

use crate::theme::{
    AMBER, BORDER, GRAPHITE_RAISED, MONITOR_BLACK, MUTED, PREVIEW, PROGRAM, TEXT, TEXT_DIM,
    routing_color,
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
    switcher: SwitcherState,
    inputs: &[InputId],
    input_names: &[String],
    select: fn(BusSelection) -> InputId,
) -> MonitorRouting {
    let desired = select(switcher.desired);
    let realized = select(switcher.realized);
    MonitorRouting {
        desired: (desired, input_name(inputs, input_names, desired)),
        realized: (realized, input_name(inputs, input_names, realized)),
    }
}

pub(crate) fn label_for_input(view: &ClientView, input: InputId) -> String {
    input_name(&view.inputs, &view.input_names, input)
}

pub(crate) fn input_name(inputs: &[InputId], input_names: &[String], input: InputId) -> String {
    inputs
        .iter()
        .position(|candidate| *candidate == input)
        .map_or_else(
            || format!("Input {input}"),
            |index| input_names[index].clone(),
        )
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

    const fn label(self) -> &'static str {
        match self {
            Self::Preview => "PREVIEW",
            Self::Program => "PROGRAM",
        }
    }

    const fn color(self) -> Color32 {
        match self {
            Self::Preview => PREVIEW,
            Self::Program => PROGRAM,
        }
    }

    fn routing(self, view: &ClientView) -> MonitorRouting {
        monitor_routing(
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
                text: "NO PREVIEW TAP · native Preview image not produced".to_owned(),
                note: "The native runtime renders Program only; this well shows routing state."
                    .to_owned(),
            }
        } else {
            MonitorCaption {
                picture: PictureMode::Hidden,
                badges: Vec::new(),
                text: "NO VIDEO FEED · waiting for encoded preview".to_owned(),
                note: format!(
                    "Routing state only; no encoded {} frame has arrived.",
                    bus.label().to_lowercase()
                ),
            }
        };
    };
    let simulated = frame.origin == MonitorFrameOrigin::Simulated;
    let (width, height) = (frame.width, frame.height);
    if picture_visible(frame.origin, frame.input, realized_input, frame.age) {
        return if simulated {
            MonitorCaption {
                picture: PictureMode::Live,
                badges: vec!["SIMULATED"],
                text: format!("SIMULATED RENDER · {width}×{height} encoded preview"),
                note: "Simulated render of the realized source only; overlays, scenes, and \
                       Fade to Black are not composited into this image."
                    .to_owned(),
            }
        } else {
            MonitorCaption {
                picture: PictureMode::Live,
                badges: Vec::new(),
                text: format!("ENCODED PREVIEW {width}×{height} · CPU readback"),
                note: format!(
                    "Native output downscaled and read back to the CPU at {width}×{height}; \
                     a low-rate monitor, not full-rate output video."
                ),
            }
        };
    }
    let text = if frame.age >= MONITOR_STALE_AFTER {
        format!(
            "STALE · last encoded frame {:.1} s old",
            frame.age.as_secs_f32()
        )
    } else {
        "STALE · encoded frame does not match the realized source".to_owned()
    };
    MonitorCaption {
        picture: PictureMode::Stale,
        badges: if simulated {
            vec!["SIMULATED", "STALE"]
        } else {
            vec!["STALE"]
        },
        text,
        note: "The last encoded frame is stale or predates the realized routing; it is dimmed \
               and is not proof of current output."
            .to_owned(),
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

/// Draws one monitor well of exactly `width` points, 16:9 plus its header.
#[allow(clippy::too_many_lines)] // one immediate-mode region
pub(crate) fn draw_monitor(
    ui: &mut Ui,
    bus: Bus,
    view: Option<&ClientView>,
    width: f32,
    picture: Option<MonitorPicture>,
    native_runtime: bool,
) {
    let routing = view.map(|view| bus.routing(view));
    let fade_to_black = bus == Bus::Program
        && view.is_some_and(|view| view.switcher.desired_fade_to_black.target_active);
    let video_height = (width * 9.0 / 16.0).round();
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, MONITOR_HEADER_HEIGHT + video_height),
        Sense::hover(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }
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
        GRAPHITE_RAISED,
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
    painter.text(
        pos2(header.min.x + 10.0, header.center().y + 1.0),
        Align2::LEFT_CENTER,
        bus.label(),
        FontId::new(12.0, FontFamily::Proportional),
        color,
    );
    if bus == Bus::Program
        && let Some(view) = view
    {
        // Confirmed active overlay channels keyed over Program (control state).
        let mut x = header.min.x + 80.0;
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
        let reserved = 90.0 + overlays as f32 * 30.0;
        let galley = crate::theme::elided(
            &painter,
            &routing.realized.1,
            FontId::new(12.0, FontFamily::Proportional),
            TEXT,
            width - reserved - 10.0,
        );
        painter.galley(
            pos2(
                header.max.x - 10.0 - galley.size().x,
                header.center().y + 1.0 - galley.size().y / 2.0,
            ),
            galley,
            TEXT,
        );
    }

    let caption = monitor_caption(
        bus,
        picture.map(|picture| picture.facts),
        routing
            .as_ref()
            .map(|routing| WireInputId::from_domain(routing.realized.0)),
        native_runtime,
    );
    painter.rect_filled(video, CornerRadius::ZERO, MONITOR_BLACK);
    if let Some(picture) = picture.filter(|_| caption.picture != PictureMode::Hidden) {
        let tint = if caption.picture == PictureMode::Live {
            Color32::WHITE
        } else {
            Color32::from_gray(70)
        };
        painter.image(
            picture.texture,
            letterbox(video, picture.facts.width, picture.facts.height),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            tint,
        );
    }
    let live = caption.picture == PictureMode::Live;
    let center = video.center();
    let title_size = (width / 22.0).clamp(15.0, 30.0);
    if !live {
        draw_safe_area(&painter, video);
        let (title, size, title_color) = match &routing {
            Some(routing) => (routing.realized.1.as_str(), title_size, TEXT_DIM),
            None => ("ROUTING STATE UNAVAILABLE", title_size * 0.7, MUTED),
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
            &format!("DESIRED: {}", routing.desired.1),
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
        painter.text(
            band.center(),
            Align2::CENTER_CENTER,
            &caption.text,
            FontId::new(10.5, FontFamily::Proportional),
            TEXT_DIM,
        );
    } else {
        painter.text(
            pos2(center.x, video.max.y - 30.0),
            Align2::CENTER_CENTER,
            &caption.text,
            FontId::new(10.5, FontFamily::Proportional),
            MUTED,
        );
        painter.text(
            pos2(center.x, video.max.y - 16.0),
            Align2::CENTER_CENTER,
            "routing state, not output video proof",
            FontId::new(9.5, FontFamily::Proportional),
            MUTED.gamma_multiply(0.75),
        );
    }
    let mut badge_right = video.max.x - 10.0;
    for badge in caption.badges.iter().rev() {
        let galley = painter.layout_no_wrap(
            (*badge).to_owned(),
            FontId::new(10.0, FontFamily::Proportional),
            Color32::BLACK,
        );
        let badge_rect = Rect::from_min_max(
            pos2(badge_right - galley.size().x - 12.0, video.min.y + 10.0),
            pos2(badge_right, video.min.y + 28.0),
        );
        let fill = if *badge == "STALE" { MUTED } else { AMBER };
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
        let badge = Rect::from_min_size(video.min + Vec2::new(10.0, 10.0), Vec2::new(44.0, 18.0));
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
        Some(routing) => format!(
            "{} desired: {}\n{} realized: {}\n{}",
            bus.label(),
            routing.desired.1,
            bus.label(),
            routing.realized.1,
            caption.note
        ),
        None => caption.note,
    };
    response.on_hover_text(tooltip);
}

/// Faint title-safe corner marks so the empty well reads as a monitor.
fn draw_safe_area(painter: &egui::Painter, video: Rect) {
    let guide = Stroke::new(1.0, BORDER);
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
