//! Client-side previews of the draft: sampled values, a wireframe proxy of
//! transformed target quads, onion skins, and a latest-wins scheduler for
//! exact (server-rendered) previews.

use core::time::Duration;

use fm_animation::{Phase, Property, Rgba, SCALE_IDENTITY_BP, Value};

use super::{
    document::{AnimDocument, TargetOutline},
    interaction::phase_state,
};

/// Most onion-skin frames shown on each side of the playhead.
pub const MAX_ONION_SKINS: u32 = 8;
/// Default delay between the last preview request and issuing it.
pub const DEFAULT_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(120);

/// One target's animated property values at a frame (only animated
/// properties are present).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimSample<T> {
    pub target: T,
    pub values: Vec<(Property, Value)>,
}

impl<T> AnimSample<T> {
    #[must_use]
    pub fn get(&self, property: Property) -> Option<Value> {
        self.values
            .iter()
            .find(|(candidate, _)| *candidate == property)
            .map(|(_, value)| *value)
    }
}

/// A target's proxy outline after animation, in canvas pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct ProxyQuad<T> {
    pub target: T,
    /// Top-left, top-right, bottom-right, bottom-left of the cropped,
    /// scaled, rotated, and translated bounds.
    pub corners: [[f32; 2]; 4],
    /// Combined static and animated opacity, `0.0..=1.0`.
    pub opacity: f32,
    pub color: Rgba,
}

impl<T> ProxyQuad<T> {
    /// The quad's centre (mean of the corners).
    #[must_use]
    pub fn center(&self) -> [f32; 2] {
        let sum = self.corners.iter().fold([0.0, 0.0], |sum, corner| {
            [sum[0] + corner[0], sum[1] + corner[1]]
        });
        [sum[0] / 4.0, sum[1] / 4.0]
    }

    /// Axis-aligned bounds `[min_x, min_y, max_x, max_y]`.
    #[must_use]
    pub fn aabb(&self) -> [f32; 4] {
        self.corners.iter().fold(
            [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
            |bounds, corner| {
                [
                    bounds[0].min(corner[0]),
                    bounds[1].min(corner[1]),
                    bounds[2].max(corner[0]),
                    bounds[3].max(corner[1]),
                ]
            },
        )
    }
}

/// Proxy quads at a frame `offset` frames from the playhead.
#[derive(Clone, Debug, PartialEq)]
pub struct OnionSkin<T> {
    pub offset: i64,
    pub frame: u32,
    pub quads: Vec<ProxyQuad<T>>,
}

/// Applies `sample` to `outline`'s static design: crop, then scale and
/// rotate about the bounds' centre, then translate.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn proxy_quad<T: Copy>(outline: &TargetOutline<T>, sample: &AnimSample<T>) -> ProxyQuad<T> {
    let scalar = |property: Property, base: i64| {
        sample
            .get(property)
            .map_or(base, |value| property.layer_scalar(base, value))
    };
    let bounds = outline.bounds;
    let (x, y) = (f64::from(bounds.x), f64::from(bounds.y));
    let (width, height) = (f64::from(bounds.width), f64::from(bounds.height));
    let crop = |property| scalar(property, 0).max(0) as f64;
    let (center_x, center_y) = (x + width / 2.0, y + height / 2.0);
    let left = x + crop(Property::CropLeft) - center_x;
    let top = y + crop(Property::CropTop) - center_y;
    let right = (x + width - crop(Property::CropRight) - center_x).max(left);
    let bottom = (y + height - crop(Property::CropBottom) - center_y).max(top);
    let identity = SCALE_IDENTITY_BP as f64;
    let scale_x = scalar(Property::ScaleX, SCALE_IDENTITY_BP) as f64 / identity;
    let scale_y = scalar(Property::ScaleY, SCALE_IDENTITY_BP) as f64 / identity;
    let radians = (scalar(Property::Rotation, outline.rotation_mdeg) as f64 / 1_000.0).to_radians();
    let (sin, cos) = radians.sin_cos();
    let offset_x = scalar(Property::PositionX, 0) as f64;
    let offset_y = scalar(Property::PositionY, 0) as f64;
    let transform = |local_x: f64, local_y: f64| {
        let (horizontal, vertical) = (local_x * scale_x, local_y * scale_y);
        [
            (center_x + offset_x + horizontal * cos - vertical * sin) as f32,
            (center_y + offset_y + horizontal * sin + vertical * cos) as f32,
        ]
    };
    let opacity =
        scalar(Property::Opacity, i64::from(outline.opacity)).clamp(0, 255) as f32 / 255.0;
    let color = sample.get(Property::Color).map_or(outline.color, |value| {
        Property::Color.layer_color(outline.color, value)
    });
    ProxyQuad {
        target: outline.target,
        corners: [
            transform(left, top),
            transform(right, top),
            transform(right, bottom),
            transform(left, bottom),
        ],
        opacity,
        color,
    }
}

impl<T: Copy + Ord> AnimDocument<T> {
    /// Every outline target's animated values at `frame` of `phase`, in
    /// outline order.
    #[must_use]
    pub fn sample_targets(&self, phase: Phase, frame: u32) -> Vec<AnimSample<T>> {
        let state = phase_state(phase, frame);
        self.targets
            .iter()
            .map(|outline| AnimSample {
                target: outline.target,
                values: self.draft.evaluate(outline.target, state),
            })
            .collect()
    }

    /// [`Self::sample_targets`] at the playhead of the shown phase.
    #[must_use]
    pub fn sample_at_playhead(&self) -> Vec<AnimSample<T>> {
        self.sample_targets(self.phase, self.playhead)
    }

    /// Proxy quads of every outline target at `frame` of the shown phase.
    #[must_use]
    pub fn proxy_at(&self, frame: u32) -> Vec<ProxyQuad<T>> {
        self.targets
            .iter()
            .zip(self.sample_targets(self.phase, frame))
            .map(|(outline, sample)| proxy_quad(outline, &sample))
            .collect()
    }

    /// Proxy quads at the playhead.
    #[must_use]
    pub fn proxy_at_playhead(&self) -> Vec<ProxyQuad<T>> {
        self.proxy_at(self.playhead)
    }

    /// Onion skins `step` frames apart, up to `count` (at most
    /// [`MAX_ONION_SKINS`]) on each side of the playhead, inside the phase;
    /// earliest first.
    #[must_use]
    pub fn onion_skins(&self, count: u32, step: u32) -> Vec<OnionSkin<T>> {
        let step = i64::from(step.max(1));
        let count = i64::from(count.min(MAX_ONION_SKINS));
        let length = i64::from(self.phase_frames());
        let playhead = i64::from(self.playhead);
        (-count..=count)
            .filter(|&index| index != 0)
            .map(|index| index * step)
            .filter(|offset| (0..=length).contains(&(playhead + offset)))
            .map(|offset| {
                let frame = u32::try_from(playhead + offset).unwrap_or(0);
                OnionSkin {
                    offset,
                    frame,
                    quads: self.proxy_at(frame),
                }
            })
            .collect()
    }

    /// The exact-preview key for the playhead of the current draft.
    #[must_use]
    pub const fn preview_key(&self) -> PreviewKey {
        PreviewKey {
            generation: self.generation,
            phase: self.phase,
            frame: self.playhead,
        }
    }
}

/// Identifies an issued exact-preview request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PreviewRequestId(pub u64);

/// What an exact preview shows: a draft generation at a frame of a phase.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PreviewKey {
    pub generation: u64,
    pub phase: Phase,
    pub frame: u32,
}

/// A request the protocol layer should send now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactPreviewRequest {
    pub id: PreviewRequestId,
    pub key: PreviewKey,
}

/// How a finished request relates to what the editor wants now.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PreviewCompletion {
    /// The result is the latest wanted preview.
    Current,
    /// A newer request is pending; the result may be shown as stale.
    Superseded,
    /// Not the request in flight (late or already cancelled); ignore it.
    Unknown,
}

/// Latest-wins, debounced exact-preview scheduling with at most one request
/// in flight. Time is a caller-supplied monotonic clock.
#[derive(Clone, Debug)]
pub struct PreviewScheduler {
    debounce: Duration,
    pending: Option<(PreviewKey, Duration)>,
    in_flight: Option<ExactPreviewRequest>,
    delivered: Option<PreviewKey>,
    next_id: u64,
}

impl Default for PreviewScheduler {
    fn default() -> Self {
        Self::new(DEFAULT_PREVIEW_DEBOUNCE)
    }
}

impl PreviewScheduler {
    #[must_use]
    pub const fn new(debounce: Duration) -> Self {
        Self {
            debounce,
            pending: None,
            in_flight: None,
            delivered: None,
            next_id: 1,
        }
    }

    /// Wants a preview of `key`, replacing any pending wish. Restarts the
    /// debounce unless `key` is already pending. A key already in flight or
    /// delivered (with nothing newer pending) needs no request.
    pub fn request(&mut self, key: PreviewKey, now: Duration) {
        if self.pending.is_some_and(|(pending, _)| pending == key) {
            return;
        }
        let in_flight = self.in_flight.map(|request| request.key);
        let latest = in_flight.or(self.delivered);
        if latest == Some(key) {
            self.pending = None;
            return;
        }
        self.pending = Some((key, now));
    }

    /// The request to send now, if the debounce elapsed and nothing is in
    /// flight.
    pub fn poll(&mut self, now: Duration) -> Option<ExactPreviewRequest> {
        if self.in_flight.is_some() {
            return None;
        }
        let (key, since) = self.pending?;
        if now < since.saturating_add(self.debounce) {
            return None;
        }
        self.pending = None;
        let request = ExactPreviewRequest {
            id: PreviewRequestId(self.next_id),
            key,
        };
        self.next_id = self.next_id.wrapping_add(1);
        self.in_flight = Some(request);
        Some(request)
    }

    /// Records the result of request `id`.
    pub fn complete(&mut self, id: PreviewRequestId) -> PreviewCompletion {
        match self.in_flight {
            Some(request) if request.id == id => {
                self.in_flight = None;
                self.delivered = Some(request.key);
                if self.pending.is_some() {
                    PreviewCompletion::Superseded
                } else {
                    PreviewCompletion::Current
                }
            }
            _ => PreviewCompletion::Unknown,
        }
    }

    /// Records that request `id` failed; its key may be requested again.
    /// Returns whether `id` was in flight.
    pub fn fail(&mut self, id: PreviewRequestId) -> bool {
        if self.in_flight.is_some_and(|request| request.id == id) {
            self.in_flight = None;
            true
        } else {
            false
        }
    }

    /// When [`Self::poll`] will next issue a request, if one is pending and
    /// nothing is in flight.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        if self.in_flight.is_some() {
            return None;
        }
        self.pending
            .map(|(_, since)| since.saturating_add(self.debounce))
    }

    #[must_use]
    pub const fn in_flight(&self) -> Option<ExactPreviewRequest> {
        self.in_flight
    }

    #[must_use]
    pub fn pending(&self) -> Option<PreviewKey> {
        self.pending.map(|(key, _)| key)
    }

    /// The key of the last completed request.
    #[must_use]
    pub const fn delivered(&self) -> Option<PreviewKey> {
        self.delivered
    }

    /// Forgets everything (for example after a reconnect); late results of
    /// earlier ids complete as [`PreviewCompletion::Unknown`].
    pub const fn reset(&mut self) {
        self.pending = None;
        self.in_flight = None;
        self.delivered = None;
    }
}
