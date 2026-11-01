//! The native Preview and Program monitor taps: the paced readbacks whose
//! frames reach Studio's monitors, and the counters they report.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

/// Encoded-fallback Preview and Program monitor taps (Protocol 2.21).
///
/// While an eligible session wants frames, every `interval`-th realized frame
/// of each tap is transformed and downscaled on the GPU into that tap's lazily
/// created readback target at the fitted monitor size, and its readback is
/// started without waiting for the GPU. Later frames poll it, never blocking;
/// a finished readback is QOI-encoded and held as that tap's atomic
/// latest-wins `monitor_frame` group. Each tap keeps at most one readback in
/// flight and skips a due frame while its previous one is still running, so a
/// slow GPU lowers the monitor rate rather than delaying Program.
///
/// Preview runs half an interval out of phase with Program, so the two taps'
/// GPU work falls on different frames. Its picture is the Preview composite
/// display outputs show (see [`preview_frame`]): the Preview source cut in.
/// During a transition that is the incoming source, as on most switchers. A
/// Preview composite is rendered for the tap only on the due frames it
/// captures (see [`preview_composite_due`]). Once the loop falls behind its
/// deadlines it is paced like display outputs: the next Preview capture, and
/// its composite, waits until the loop has spent several times the last
/// one's cost on everything else, so a costly Preview composite lowers the
/// Preview rate rather than the Program rate.
///
/// Readback or encode failures skip the frame and are counted in telemetry;
/// they never fail the frame tick.
#[cfg(feature = "native-media")]
pub(crate) struct NativeMonitorTap {
    pub(crate) dimensions: Option<(u16, u16)>,
    pub(crate) interval: u64,
    pub(crate) demand: bool,
    /// Per tap, in [`MonitorTap::index`] order.
    pub(crate) taps: [NativeTapSlot; 2],
}

/// One native monitor tap's readback target, readback in flight, and latest
/// encoded group.
#[cfg(feature = "native-media")]
#[derive(Default)]
pub(crate) struct NativeTapSlot {
    pub(crate) readback: Option<NativeProgramReadback>,
    pub(crate) in_flight: Option<NativeTapReadback>,
    pub(crate) sequence: u64,
    pub(crate) pending: Option<Vec<u8>>,
}

/// A started tap readback and the frame it shows.
#[cfg(feature = "native-media")]
pub(crate) struct NativeTapReadback {
    pub(crate) pending: fm_gpu::NativePendingReadback,
    pub(crate) frame: u64,
    pub(crate) input: InputId,
}

/// Monitor tap counters for one tap.
#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeTapCounters {
    /// Encoded groups made ready to publish.
    pub(crate) published: u64,
    /// Frames whose render, readback, or encode failed.
    pub(crate) failed: u64,
    /// Due frames skipped: the loop was behind its deadline or the tap's
    /// previous readback was still in flight.
    pub(crate) skipped: u64,
}

#[cfg(feature = "native-media")]
impl NativeMonitorTap {
    /// Native monitor frames target at most about ten per second per tap.
    const TARGET_FPS: u64 = 10;

    /// Both taps' readback targets and SDR transforms are created here, at
    /// startup, so a Studio peer connecting never stalls a frame on their
    /// pipeline compilation; one that fails is retried on a due frame.
    pub(crate) fn new(runtime: &NativeMediaRuntime, settings: &fm_model::ProjectSettings) -> Self {
        let dimensions = settings.video.dimensions;
        let rate = settings.frame_rate;
        let whole_fps = u64::from(rate.numerator()).div_ceil(u64::from(rate.denominator().max(1)));
        let mut tap = Self {
            dimensions: monitor_tap::fit_monitor(dimensions.width(), dimensions.height()),
            interval: whole_fps.div_ceil(Self::TARGET_FPS).max(1),
            demand: false,
            taps: [NativeTapSlot::default(), NativeTapSlot::default()],
        };
        if let Some((width, height)) = tap.dimensions {
            for slot in &mut tap.taps {
                slot.prepare(runtime, width, height);
            }
        }
        tap
    }

    /// The frame interval between this monitor's taps.
    pub(crate) const fn interval(&self) -> u64 {
        self.interval
    }

    /// Sets whether any peer wants monitor frames.
    pub(crate) const fn set_demand(&mut self, demand: bool) {
        self.demand = demand;
    }

    /// Whether realized frame `frame` falls due for `tap`: there is demand
    /// and the frame falls on the tap's phase.
    pub(crate) fn due(&self, tap: MonitorTap, frame: u64) -> bool {
        let phase = match tap {
            MonitorTap::Program => 0,
            MonitorTap::Preview => self.interval / 2,
        };
        self.demand
            && self.dimensions.is_some()
            && frame.wrapping_add(phase).is_multiple_of(self.interval)
    }

    /// Whether `tap`'s previous readback has not finished yet.
    pub(crate) fn busy(&self, tap: MonitorTap) -> bool {
        self.taps[tap.index()].in_flight.is_some()
    }

    /// Polls every tap's readback in flight once, without blocking, and
    /// encodes the finished ones.
    pub(crate) fn harvest(
        &mut self,
        runtime: &NativeMediaRuntime,
        server: &ServerIdentity,
        counters: &mut [NativeTapCounters; 2],
    ) {
        let Some((width, height)) = self.dimensions else {
            return;
        };
        for tap in MonitorTap::ALL {
            let slot = &mut self.taps[tap.index()];
            let Some(started) = slot.in_flight.take() else {
                continue;
            };
            let published = match runtime.poll_program_readback(started.pending) {
                Ok(fm_gpu::NativeReadbackPoll::Pending(pending)) => {
                    slot.in_flight = Some(NativeTapReadback { pending, ..started });
                    continue;
                }
                Ok(fm_gpu::NativeReadbackPoll::Ready(pixels)) => slot.encode(
                    server,
                    tap,
                    started.frame,
                    started.input,
                    width,
                    height,
                    &pixels,
                ),
                Err(_) => false,
            };
            let tap_counters = &mut counters[tap.index()];
            if published {
                tap_counters.published = tap_counters.published.saturating_add(1);
            } else {
                tap_counters.failed = tap_counters.failed.saturating_add(1);
            }
        }
    }

    /// Starts `tap`'s readback of `texture`, realized frame `frame` showing
    /// `input`, when the tap is due on it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn capture(
        &mut self,
        tap: MonitorTap,
        runtime: &NativeMediaRuntime,
        texture: Option<&NativeTexture>,
        frame: u64,
        input: InputId,
        counters: &mut [NativeTapCounters; 2],
    ) {
        if !self.due(tap, frame) {
            return;
        }
        if self.busy(tap) {
            Self::skip(tap, counters);
            return;
        }
        let Some((width, height)) = self.dimensions else {
            return;
        };
        // A due frame without a picture is a failed render.
        let started = texture.is_some_and(|texture| {
            self.taps[tap.index()].begin(runtime, texture, frame, input, width, height)
        });
        if !started {
            let tap_counters = &mut counters[tap.index()];
            tap_counters.failed = tap_counters.failed.saturating_add(1);
        }
    }

    /// Counts a due frame of `tap` that was not captured.
    pub(crate) fn skip(tap: MonitorTap, counters: &mut [NativeTapCounters; 2]) {
        let tap_counters = &mut counters[tap.index()];
        tap_counters.skipped = tap_counters.skipped.saturating_add(1);
    }

    /// Every tap's latest encoded group, Preview first.
    pub(crate) fn take_frames(&mut self) -> Vec<(MonitorTap, Vec<u8>)> {
        MonitorTap::ALL
            .into_iter()
            .filter_map(|tap| {
                self.taps[tap.index()]
                    .pending
                    .take()
                    .map(|bytes| (tap, bytes))
            })
            .collect()
    }
}

#[cfg(feature = "native-media")]
impl NativeTapSlot {
    /// Creates the tap's readback target and transform if it has none.
    pub(crate) fn prepare(&mut self, runtime: &NativeMediaRuntime, width: u16, height: u16) {
        if self.readback.is_some() {
            return;
        }
        if let (Some(width), Some(height)) = (
            NonZeroU32::new(u32::from(width)),
            NonZeroU32::new(u32::from(height)),
        ) {
            self.readback = runtime.create_program_readback_blocking(width, height).ok();
        }
    }

    pub(crate) fn begin(
        &mut self,
        runtime: &NativeMediaRuntime,
        texture: &NativeTexture,
        frame: u64,
        input: InputId,
        width: u16,
        height: u16,
    ) -> bool {
        self.prepare(runtime, width, height);
        let Some(readback) = self.readback.as_ref() else {
            return false;
        };
        match runtime.begin_program_readback(readback, texture) {
            Ok(pending) => {
                self.in_flight = Some(NativeTapReadback {
                    pending,
                    frame,
                    input,
                });
                true
            }
            Err(_) => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode(
        &mut self,
        server: &ServerIdentity,
        tap: MonitorTap,
        frame: u64,
        input: InputId,
        width: u16,
        height: u16,
        pixels: &fm_gpu::NativeTextureReadback,
    ) -> bool {
        let (Ok(stride), Some(sequence)) =
            (usize::try_from(pixels.stride), self.sequence.checked_add(1))
        else {
            return false;
        };
        if pixels.width != u32::from(width) || pixels.height != u32::from(height) {
            return false;
        }
        let picture = monitor_tap::MonitorPicture {
            tap,
            sequence,
            frame,
            input,
            origin: fm_protocol::MonitorFrameOrigin::Native,
            width,
            height,
            pixels: &pixels.bytes,
            stride,
            bytes_per_pixel: 4,
        };
        let Some(bytes) = monitor_tap::encode_picture(server, &picture) else {
            return false;
        };
        self.sequence = sequence;
        self.pending = Some(bytes);
        true
    }
}

/// Whether realized frame `frame` is due for `tap` at the control loop's
/// monitor interval and demand.
#[cfg(feature = "native-media")]
pub(crate) fn monitor_due(
    interval: Option<u64>,
    demand: bool,
    tap: MonitorTap,
    frame: u64,
) -> bool {
    let Some(interval) = interval else {
        return false;
    };
    let phase = match tap {
        MonitorTap::Program => 0,
        MonitorTap::Preview => interval / 2,
    };
    demand && frame.wrapping_add(phase).is_multiple_of(interval)
}

/// Whether a realized frame renders the Preview composite: only for a
/// consumer that takes it this frame, a display output showing Preview
/// (`shows_preview`) that presents it (`display_due`), or the Preview tap's
/// capture (`tap_captures`). Without a presenting display `display_due` holds
/// on every frame, so it alone must not render the composite of a due tap
/// frame that a late loop paced out: that blocking composite is most of what
/// pacing the tap saves.
#[cfg(feature = "native-media")]
pub(crate) const fn preview_composite_due(
    shows_preview: bool,
    display_due: bool,
    tap_captures: bool,
) -> bool {
    (shows_preview && display_due) || tap_captures
}

/// The source the Preview monitor shows with `frame`: during a transition
/// (timed, T-bar, or Stinger) its incoming source, as most switchers' Preview
/// does, otherwise the realized Preview input.
#[cfg(feature = "native-media")]
pub(crate) fn preview_monitor_input(frame: &FrameResult, realized_preview: InputId) -> InputId {
    frame.program.secondary.unwrap_or(realized_preview)
}
