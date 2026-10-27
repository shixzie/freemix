//! AU-005: once warmed up, the live Master mix renders without touching the
//! heap through the entry point the native daemon calls
//! (`mix_planar_timed_into_with_monitor`), with every strip DSP stage
//! switched on (delay, polarity, high-pass, the five EQ bands, gate,
//! expander, compressor, limiter), the Master chain, per-input sample
//! delay, a Master bus insert (a stinger's own audio), the monitor bus, the
//! strip, Master, and monitor meters, and per-strip and Program BS.1770
//! loudness.
//!
//! The workspace forbids `unsafe`, so the counting global allocator is
//! `stats_alloc`'s: it wraps the system allocator and counts every
//! allocation, reallocation, and free. This target holds one test, so no
//! other test thread allocates while it measures.

use std::alloc::System;
use std::num::NonZeroU128;

use fm_audio::{
    ChannelMapping, ChannelMeter, InputState, LoudnessMeter, MasterBusInsert, MasterMixer,
    MixerMeterSlices, PlanarAudioSource, SourceGain,
};
use fm_frame::{
    ClockDomainId, MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp,
    SequenceNumber,
};
use fm_types::strip_dsp::{StripDelay, StripDsp, StripDspPreset};
use fm_types::{
    AudioFormat, ChannelLayout, InputId, MediaTimestamp, SampleFormat, SampleRate, TimeBase,
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const RATE: u32 = 48_000;
/// One 50 fps frame of samples.
const BLOCK: usize = 960;
/// Blocks rendered before measuring, so every lazily sized buffer and
/// every delay line has filled.
const WARM_UP_BLOCKS: usize = 100;
/// Blocks measured: ten seconds of Program audio.
const MEASURED_BLOCKS: usize = 500;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn stereo() -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(RATE).unwrap(),
        sample_format: SampleFormat::F32,
        channels: ChannelLayout::stereo(),
    }
}

fn timing(block: usize) -> MediaTiming {
    let start = u64::try_from(block * BLOCK).unwrap();
    let samples = u64::try_from(BLOCK).unwrap();
    MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(i64::try_from(start).unwrap()),
            TimeBase::new(1, RATE).unwrap(),
        ),
        NormalizedTimestamp::from_nanos(
            i64::try_from(start * 1_000_000_000 / u64::from(RATE)).unwrap(),
        ),
        NormalizedDuration::from_nanos(samples * 1_000_000_000 / u64::from(RATE)).unwrap(),
        ClockDomainId::new(NonZeroU128::new(7).unwrap()),
        SequenceNumber::new(u64::try_from(block).unwrap()),
    )
    .unwrap()
}

/// Every stage of a strip chain switched on.
fn full_chain() -> StripDsp {
    let mut chain = StripDspPreset::Voice.chain();
    chain.delay = StripDelay {
        enabled: true,
        delay_us: 20_000,
    };
    chain.invert_polarity = true;
    chain.high_pass.enabled = true;
    chain.eq.enabled = true;
    for band in &mut chain.eq.bands {
        band.enabled = true;
    }
    chain.gate.enabled = true;
    chain.expander.enabled = true;
    chain.compressor.enabled = true;
    chain.limiter = StripDspPreset::Master.chain().limiter;
    chain.limiter.enabled = true;
    chain
}

/// Fills `planes` with the next block of a sine at `frequency` Hz, in place.
fn tone(planes: &mut [Vec<f32>], frequency: f64, amplitude: f32, block: usize) {
    for (offset, sample) in planes[0].iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)] // sample positions stay exact in f64
        let time = (block * BLOCK + offset) as f64 / f64::from(RATE);
        #[allow(clippy::cast_possible_truncation)] // a sine is in [-1, 1]
        let value = (core::f64::consts::TAU * frequency * time).sin() as f32;
        *sample = amplitude * value;
    }
    let (left, right) = planes.split_at_mut(1);
    right[0].copy_from_slice(&left[0]);
}

#[test]
fn the_master_mix_with_every_dsp_stage_renders_without_allocating() {
    let format = stereo();
    let layout = ChannelLayout::stereo();
    let ids = [input(1), input(2), input(3)];
    let mut mixer = MasterMixer::new(format.clone()).unwrap();
    for (index, &id) in ids.iter().enumerate() {
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(format.channels.clone()).unwrap(),
                InputState {
                    follow_video: false,
                    ..InputState::default()
                },
            )
            .unwrap();
        mixer.set_input_dsp(id, &full_chain()).unwrap();
        mixer.set_input_delay(id, 240 * (index + 1)).unwrap();
    }
    let mut master_chain = StripDspPreset::Master.chain();
    master_chain.eq.enabled = true;
    mixer.set_master_dsp(&master_chain).unwrap();
    let mut loudness = LoudnessMeter::new(RATE, &layout).unwrap();
    let mut strip_loudness: Vec<LoudnessMeter> = ids
        .iter()
        .map(|_| LoudnessMeter::new(RATE, &layout).unwrap())
        .collect();
    let mut sting = vec![vec![0.0; BLOCK]; 2];
    let mut monitor = vec![vec![0.0; BLOCK]; 2];
    let mut monitor_meters = [ChannelMeter::default(); 2];

    let mut sources: Vec<Vec<Vec<f32>>> = ids.iter().map(|_| vec![vec![0.0; BLOCK]; 2]).collect();
    let mut output = vec![vec![0.0; BLOCK]; 2];
    let mut master = [ChannelMeter::default(); 2];
    let mut input_meters = vec![ChannelMeter::default(); ids.len() * 2];
    let mut render = |block: usize,
                      sources: &mut [Vec<Vec<f32>>],
                      output: &mut [Vec<f32>],
                      loudness: &mut LoudnessMeter| {
        for (index, planes) in sources.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)] // three small indices
            let frequency = 220.0 * (index + 1) as f64;
            tone(planes, frequency, 0.3, block);
        }
        tone(&mut sting, 880.0, 0.1, block);
        let submissions = [0, 1, 2].map(|index| PlanarAudioSource {
            input: ids[index],
            sample_rate: SampleRate::new(RATE).unwrap(),
            channel_layout: &layout,
            planes: &sources[index],
            samples: BLOCK,
            source_gain: SourceGain::UNITY,
        });
        mixer
            .mix_planar_timed_into_with_monitor(
                timing(block),
                BLOCK,
                &submissions,
                &ids[..1],
                output,
                &mut monitor,
                MixerMeterSlices {
                    master: &mut master,
                    inputs: &mut input_meters,
                    monitor: &mut monitor_meters,
                    loudness: Some(&mut strip_loudness),
                },
                Some(MasterBusInsert {
                    planes: &sting,
                    replace: false,
                }),
            )
            .unwrap();
        for id in ids {
            assert!(mixer.input_dynamics(id).is_some());
        }
        loudness.process_planes(output, BLOCK);
        loudness.reading()
    };

    for block in 0..WARM_UP_BLOCKS {
        render(block, &mut sources, &mut output, &mut loudness);
    }
    let region = Region::new(GLOBAL);
    let mut last = None;
    for block in WARM_UP_BLOCKS..WARM_UP_BLOCKS + MEASURED_BLOCKS {
        last = Some(render(block, &mut sources, &mut output, &mut loudness));
    }
    let change = region.change();

    // The mix is not silent: the measured path did the work.
    assert!(master[0].peak > 0.01, "{master:?}");
    assert!(last.is_some_and(|reading| reading.momentary_lufs.is_some()));
    assert_eq!(
        (
            change.allocations,
            change.reallocations,
            change.deallocations
        ),
        (0, 0, 0),
        "the render path touched the heap: {change:?}"
    );
}
