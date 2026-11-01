//! Strip and Master DSP editing (AU-005, Protocol 2.75).
//!
//! [`StripDspBinding`] binds the FX editor to one chain the authority
//! owns. An operator edit becomes a draft the editor shows instead of the
//! authority chain; [`StripDspBinding::poll`] sends it: at most every
//! [`DRAG_SEND_INTERVAL`] while a knob or curve handle is dragged (latest
//! wins), and at once when it is released or for a click. Every set of one
//! drag carries the drag's gesture token, so the server folds the whole
//! drag into one undo step. The draft is dropped once the authority reports
//! it, or [`REFUSAL_GRACE`] after the last send when the server refused it,
//! so the editor never shows a chain the server does not apply.
//!
//! [`response_db`] is the analytic magnitude response the editor draws for
//! the high-pass filter and the EQ bands, with the same RBJ cookbook filters
//! the native mixer runs.

use core::num::NonZeroU64;
use std::time::{Duration, Instant};

use fm_protocol::{AudioDspTarget, AudioMixerOperation, GradeGesture, WireInputId};
use fm_types::strip_dsp::{EqBandKind, StripDsp, StripDspTarget};

/// Shortest gap between two chain sets sent during one drag.
pub const DRAG_SEND_INTERVAL: Duration = Duration::from_millis(80);
/// How long a sent draft waits for the authority before the editor shows
/// the authority chain again (the server refused it).
pub const REFUSAL_GRACE: Duration = Duration::from_secs(2);
/// The high-pass filter's Q (Butterworth), as the native mixer runs it.
const HIGH_PASS_Q: f64 = core::f64::consts::FRAC_1_SQRT_2;

/// The wire target of a chain target.
#[must_use]
pub const fn wire_target(target: StripDspTarget) -> AudioDspTarget {
    match target {
        StripDspTarget::Master => AudioDspTarget::Master,
        StripDspTarget::Input(input) => AudioDspTarget::Input(WireInputId::from_domain(input)),
        StripDspTarget::Aux(bus) => AudioDspTarget::Aux(bus),
    }
}

/// The mixer operation that sets `target`'s chain.
#[must_use]
pub fn set_operation(
    target: StripDspTarget,
    chain: StripDsp,
    gesture: Option<GradeGesture>,
) -> AudioMixerOperation {
    AudioMixerOperation::SetStripDsp {
        target: wire_target(target),
        chain: Box::new(chain),
        gesture,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Gesture {
    token: NonZeroU64,
    /// Whether a set of this drag was sent, so later sets continue it.
    sent: bool,
}

/// The FX editor's draft of one chain; see the module docs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StripDspBinding {
    target: StripDspTarget,
    draft: Option<StripDsp>,
    /// The draft changed since it was last sent.
    dirty: bool,
    gesture: Option<Gesture>,
    last_sent: Option<Instant>,
}

impl StripDspBinding {
    /// A binding with no draft.
    #[must_use]
    pub const fn new(target: StripDspTarget) -> Self {
        Self {
            target,
            draft: None,
            dirty: false,
            gesture: None,
            last_sent: None,
        }
    }

    /// The chain this binding edits.
    #[must_use]
    pub const fn target(&self) -> StripDspTarget {
        self.target
    }

    /// The chain the editor shows: the draft while there is one, else the
    /// authority's.
    #[must_use]
    pub fn shown(&self, authority: &StripDsp) -> StripDsp {
        self.draft.unwrap_or(*authority)
    }

    /// Whether an edit is not yet confirmed by the authority.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.draft.is_some()
    }

    /// Records the operator's edited chain.
    pub fn edit(&mut self, chain: StripDsp) {
        if self.draft != Some(chain) {
            self.draft = Some(chain);
            self.dirty = true;
        }
    }

    /// The set to send now, if any; `dragging` while a pointer is held on
    /// the editor. Drops the draft once `authority` reports it, or
    /// [`REFUSAL_GRACE`] after the last send.
    ///
    /// # Panics
    ///
    /// Never: gesture tokens are nonzero by construction.
    pub fn poll(
        &mut self,
        authority: &StripDsp,
        dragging: bool,
        now: Instant,
    ) -> Option<AudioMixerOperation> {
        let Some(chain) = self.draft else {
            self.gesture = None;
            return None;
        };
        if dragging {
            if !self.dirty {
                return None;
            }
            let gesture = *self.gesture.get_or_insert_with(|| Gesture {
                token: crate::scene_designer::document::fresh_gesture()
                    .try_into()
                    .expect("gesture tokens are nonzero"),
                sent: false,
            });
            if gesture.sent
                && self
                    .last_sent
                    .is_some_and(|sent| now.saturating_duration_since(sent) < DRAG_SEND_INTERVAL)
            {
                return None;
            }
            self.gesture = Some(Gesture {
                sent: true,
                ..gesture
            });
            self.dirty = false;
            self.last_sent = Some(now);
            return Some(set_operation(
                self.target,
                chain,
                Some(GradeGesture {
                    token: gesture.token,
                    continues: gesture.sent,
                }),
            ));
        }
        let gesture = self.gesture.take();
        if self.dirty {
            self.dirty = false;
            self.last_sent = Some(now);
            return Some(set_operation(
                self.target,
                chain,
                gesture.map(|gesture| GradeGesture {
                    token: gesture.token,
                    continues: gesture.sent,
                }),
            ));
        }
        if chain == *authority
            || self
                .last_sent
                .is_none_or(|sent| now.saturating_duration_since(sent) >= REFUSAL_GRACE)
        {
            self.draft = None;
        }
        None
    }
}

/// One biquad's magnitude at `frequency_hz`, in dB.
fn biquad_db(coefficients: [f64; 5], frequency_hz: f64, sample_rate_hz: f64) -> f64 {
    let [b0, b1, b2, a1, a2] = coefficients;
    let omega = core::f64::consts::TAU * frequency_hz / sample_rate_hz;
    let (sin1, cos1) = omega.sin_cos();
    let (sin2, cos2) = (2.0 * omega).sin_cos();
    let numerator = (b0 + b1 * cos1 + b2 * cos2).hypot(b1 * sin1 + b2 * sin2);
    let denominator = (1.0 + a1 * cos1 + a2 * cos2).hypot(a1 * sin1 + a2 * sin2);
    20.0 * (numerator / denominator).max(1e-9).log10()
}

/// RBJ cookbook coefficients, normalized: `[b0, b1, b2, a1, a2]`.
fn rbj(
    kind: Option<EqBandKind>,
    frequency_hz: f64,
    q: f64,
    gain_db: f64,
    sample_rate_hz: f64,
) -> [f64; 5] {
    let frequency = frequency_hz.min(sample_rate_hz * 0.45);
    let omega = core::f64::consts::TAU * frequency / sample_rate_hz;
    let (sin_omega, cos_omega) = omega.sin_cos();
    let alpha = sin_omega / (2.0 * q);
    let amplitude = 10_f64.powf(gain_db / 40.0);
    let (b0, b1, b2, a0, a1, a2) = match kind {
        None => (
            f64::midpoint(1.0, cos_omega),
            -(1.0 + cos_omega),
            f64::midpoint(1.0, cos_omega),
            1.0 + alpha,
            -2.0 * cos_omega,
            1.0 - alpha,
        ),
        Some(EqBandKind::Peak) => (
            1.0 + alpha * amplitude,
            -2.0 * cos_omega,
            1.0 - alpha * amplitude,
            1.0 + alpha / amplitude,
            -2.0 * cos_omega,
            1.0 - alpha / amplitude,
        ),
        Some(shelf_kind) => {
            let shelf = 2.0 * amplitude.sqrt() * alpha;
            let plus = (amplitude + 1.0) + (amplitude - 1.0) * cos_omega;
            let minus = (amplitude + 1.0) - (amplitude - 1.0) * cos_omega;
            if shelf_kind == EqBandKind::LowShelf {
                (
                    amplitude * (minus + shelf),
                    2.0 * amplitude * ((amplitude - 1.0) - (amplitude + 1.0) * cos_omega),
                    amplitude * (minus - shelf),
                    plus + shelf,
                    -2.0 * ((amplitude - 1.0) + (amplitude + 1.0) * cos_omega),
                    plus - shelf,
                )
            } else {
                (
                    amplitude * (plus + shelf),
                    -2.0 * amplitude * ((amplitude - 1.0) + (amplitude + 1.0) * cos_omega),
                    amplitude * (plus - shelf),
                    minus + shelf,
                    2.0 * ((amplitude - 1.0) - (amplitude + 1.0) * cos_omega),
                    minus - shelf,
                )
            }
        }
    };
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// The chain's filter response at `frequency_hz`, in dB: the high-pass
/// filter and every enabled EQ band while the EQ is on, ignoring the chain
/// bypass (the editor dims a bypassed curve instead).
#[must_use]
pub fn response_db(chain: &StripDsp, frequency_hz: f64, sample_rate_hz: f64) -> f64 {
    let mut db = 0.0;
    if chain.high_pass.enabled {
        db += biquad_db(
            rbj(
                None,
                f64::from(chain.high_pass.frequency_hz),
                HIGH_PASS_Q,
                0.0,
                sample_rate_hz,
            ),
            frequency_hz,
            sample_rate_hz,
        );
    }
    if chain.eq.enabled {
        for band in chain.eq.bands.iter().filter(|band| band.enabled) {
            db += biquad_db(
                rbj(
                    Some(band.kind),
                    f64::from(band.frequency_hz),
                    f64::from(band.q_milli) / 1_000.0,
                    f64::from(band.gain_millidb) / 1_000.0,
                    sample_rate_hz,
                ),
                frequency_hz,
                sample_rate_hz,
            );
        }
    }
    db
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_types::strip_dsp::StripDspPreset;

    fn input(value: u128) -> fm_types::InputId {
        fm_types::InputId::new(core::num::NonZeroU128::new(value).unwrap())
    }

    fn chain_of(operation: &AudioMixerOperation) -> (StripDsp, Option<GradeGesture>) {
        let AudioMixerOperation::SetStripDsp { chain, gesture, .. } = operation else {
            panic!("{operation:?}");
        };
        (**chain, *gesture)
    }

    #[test]
    fn a_click_sends_at_once_and_the_draft_waits_for_the_authority() {
        let target = StripDspTarget::Input(input(3));
        let mut binding = StripDspBinding::new(target);
        let authority = StripDsp::DEFAULT;
        let now = Instant::now();
        assert_eq!(binding.poll(&authority, false, now), None);
        let voice = StripDspPreset::Voice.chain();
        binding.edit(voice);
        assert_eq!(binding.shown(&authority), voice);
        let sent = binding.poll(&authority, false, now).unwrap();
        assert_eq!(chain_of(&sent), (voice, None));
        let AudioMixerOperation::SetStripDsp { target: wire, .. } = &sent else {
            unreachable!()
        };
        assert_eq!(
            *wire,
            AudioDspTarget::Input(WireInputId::from_domain(input(3)))
        );
        // Nothing more to send; the draft stays until the authority has it.
        assert_eq!(binding.poll(&authority, false, now), None);
        assert!(binding.is_pending());
        assert_eq!(binding.poll(&voice, false, now), None);
        assert!(!binding.is_pending());
        assert_eq!(binding.shown(&authority), authority);
    }

    #[test]
    fn a_refused_draft_gives_way_to_the_authority() {
        let mut binding = StripDspBinding::new(StripDspTarget::Master);
        let authority = StripDsp::DEFAULT;
        let now = Instant::now();
        binding.edit(StripDspPreset::Master.chain());
        assert!(binding.poll(&authority, false, now).is_some());
        assert_eq!(
            binding.poll(&authority, false, now + REFUSAL_GRACE / 2),
            None
        );
        assert!(binding.is_pending());
        assert_eq!(binding.poll(&authority, false, now + REFUSAL_GRACE), None);
        assert!(!binding.is_pending());
    }

    #[test]
    fn a_drag_is_throttled_and_carries_one_gesture() {
        let mut binding = StripDspBinding::new(StripDspTarget::Input(input(1)));
        let authority = StripDsp::DEFAULT;
        let start = Instant::now();
        let mut chain = StripDsp::DEFAULT;
        chain.compressor.enabled = true;
        let mut sent = Vec::new();
        for step in 0..10_u32 {
            chain.compressor.threshold_millidb = -10_000 - i32::try_from(step).unwrap() * 1_000;
            binding.edit(chain);
            let now = start + Duration::from_millis(u64::from(step) * 20);
            if let Some(operation) = binding.poll(&authority, true, now) {
                sent.push(chain_of(&operation));
            }
        }
        // 10 edits over 180 ms: the first, then one per 80 ms.
        assert_eq!(sent.len(), 3, "{sent:?}");
        let token = sent[0].1.unwrap().token;
        assert!(!sent[0].1.unwrap().continues);
        assert!(sent[1..].iter().all(|(_, gesture)| *gesture
            == Some(GradeGesture {
                token,
                continues: true
            })));
        // Releasing sends the last value at once, continuing the drag.
        let released = binding
            .poll(&authority, false, start + Duration::from_millis(190))
            .unwrap();
        assert_eq!(
            chain_of(&released),
            (
                chain,
                Some(GradeGesture {
                    token,
                    continues: true
                })
            )
        );
        // The next drag has its own gesture.
        chain.compressor.ratio_milli = 6_000;
        binding.edit(chain);
        let next = binding
            .poll(&authority, true, start + Duration::from_millis(400))
            .unwrap();
        let gesture = chain_of(&next).1.unwrap();
        assert_ne!(gesture.token, token);
        assert!(!gesture.continues);
    }

    #[test]
    fn the_response_curve_follows_the_bands_and_the_high_pass() {
        let mut chain = StripDsp::DEFAULT;
        assert!(response_db(&chain, 1_000.0, 48_000.0).abs() < 1e-9);
        chain.eq.enabled = true;
        let band = &mut chain.eq.bands[2];
        band.enabled = true;
        band.kind = EqBandKind::Peak;
        band.frequency_hz = 1_000;
        band.gain_millidb = 9_000;
        band.q_milli = 1_000;
        assert!((response_db(&chain, 1_000.0, 48_000.0) - 9.0).abs() < 0.01);
        assert!(response_db(&chain, 20.0, 48_000.0).abs() < 0.1);
        // A disabled EQ draws only the filter: -3 dB at the corner, about
        // 12 dB per octave below it.
        chain.eq.enabled = false;
        chain.high_pass.enabled = true;
        chain.high_pass.frequency_hz = 200;
        assert!((response_db(&chain, 200.0, 48_000.0) + 3.0).abs() < 0.2);
        assert!(response_db(&chain, 50.0, 48_000.0) < -20.0);
        assert!(response_db(&chain, 1_000.0, 48_000.0).abs() < 0.3);
    }
}
