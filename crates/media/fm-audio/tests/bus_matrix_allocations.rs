//! AU-002: once warmed up, the aux bus matrix renders without touching the
//! heap: four strips with pre- and post-fader sends on every bus, bus DSP
//! (high-pass, compressor, limiter) on two buses, and mix-minus and plain
//! destinations, with the per-bus meters.
//!
//! The workspace forbids `unsafe`, so the counting global allocator is
//! `stats_alloc`'s: it wraps the system allocator and counts every
//! allocation, reallocation, and free. This target holds one test, so no
//! other test thread allocates while it measures.

use std::alloc::System;
use std::num::NonZeroU128;

use fm_audio::{
    BUS_COUNT, BusDestination, BusMatrix, BusSend, BusStripInput, DestinationId, Gain, MixBus,
    SendTap, StripDsp,
};
use fm_types::{InputId, SampleRate};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// One 50 fps frame of samples.
const BLOCK: usize = 960;
const STRIPS: usize = 4;
const WARM_UP_BLOCKS: usize = 100;
/// Ten seconds of audio.
const MEASURED_BLOCKS: usize = 500;

fn strip(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn bus_dsp() -> StripDsp {
    let mut dsp = StripDsp::new(48_000.0, CHANNELS).unwrap();
    dsp.set_high_pass(80.0, 0.707).unwrap();
    dsp.set_compressor(-18.0, 4.0, 5.0, 50.0, 6.0, 3.0).unwrap();
    dsp.set_limiter(-1.0, 50.0).unwrap();
    dsp
}

/// Every strip sends to every aux bus (alternating pre- and post-fader),
/// two buses process, and three destinations listen (two of them
/// mix-minus).
fn configured_matrix(ids: &[InputId]) -> BusMatrix {
    let mut matrix = BusMatrix::new(SampleRate::new(RATE).unwrap(), CHANNELS).unwrap();
    for (index, &id) in ids.iter().enumerate() {
        matrix.add_strip(id).unwrap();
        for bus in &MixBus::ALL[1..] {
            let tap = if index % 2 == 0 {
                SendTap::PreFader
            } else {
                SendTap::PostFader
            };
            matrix
                .set_send(
                    id,
                    *bus,
                    BusSend {
                        enabled: true,
                        level: Gain::from_linear(0.5).unwrap(),
                        tap,
                    },
                )
                .unwrap();
        }
    }
    matrix
        .set_bus_processing(MixBus::A, Some(bus_dsp()))
        .unwrap();
    matrix
        .set_bus_processing(MixBus::E, Some(bus_dsp()))
        .unwrap();
    for (number, (bus, mix_minus)) in [
        (MixBus::Master, None),
        (MixBus::A, Some(ids[0])),
        (MixBus::E, Some(ids[1])),
    ]
    .into_iter()
    .enumerate()
    {
        matrix
            .add_destination(BusDestination {
                id: DestinationId::Network(u32::try_from(number).unwrap()),
                bus,
                mix_minus,
            })
            .unwrap();
    }
    matrix
}

#[test]
fn the_bus_matrix_with_processing_and_mix_minus_renders_without_allocating() {
    let ids: Vec<InputId> = (1..=u128::try_from(STRIPS).unwrap()).map(strip).collect();
    let mut matrix = configured_matrix(&ids);

    let mut sources = vec![vec![vec![0.0_f32; BLOCK]; CHANNELS]; STRIPS];
    let mut buses = vec![vec![0.0_f32; BLOCK]; BUS_COUNT * CHANNELS];
    let mut destinations = vec![vec![0.0_f32; BLOCK]; 3 * CHANNELS];
    let mut render = |block: usize,
                      sources: &mut [Vec<Vec<f32>>],
                      buses: &mut [Vec<f32>],
                      destinations: &mut [Vec<f32>]| {
        for (index, planes) in sources.iter_mut().enumerate() {
            for plane in planes.iter_mut() {
                for (offset, sample) in plane.iter_mut().enumerate() {
                    #[allow(clippy::cast_precision_loss)] // sample positions stay exact
                    let time = (block * BLOCK + offset) as f64 / f64::from(RATE);
                    #[allow(clippy::cast_precision_loss)] // four small indices
                    let frequency = 110.0 * (index + 1) as f64;
                    #[allow(clippy::cast_possible_truncation)] // a sine is in [-1, 1]
                    let value = (core::f64::consts::TAU * frequency * time).sin() as f32;
                    *sample = 0.2 * value;
                }
            }
        }
        let planes: [[&[f32]; CHANNELS]; STRIPS] = core::array::from_fn(|index| {
            core::array::from_fn(|channel| sources[index][channel].as_slice())
        });
        let inputs: [BusStripInput<'_>; STRIPS] = core::array::from_fn(|index| BusStripInput {
            strip: ids[index],
            planes: &planes[index],
        });
        let mut bus_out: [&mut [f32]; BUS_COUNT * CHANNELS] = {
            let mut planes = buses.iter_mut();
            core::array::from_fn(|_| planes.next().unwrap().as_mut_slice())
        };
        let mut destination_out: [&mut [f32]; 3 * CHANNELS] = {
            let mut planes = destinations.iter_mut();
            core::array::from_fn(|_| planes.next().unwrap().as_mut_slice())
        };
        matrix
            .render(&inputs, &mut bus_out, &mut destination_out)
            .unwrap()
    };

    for block in 0..WARM_UP_BLOCKS {
        render(block, &mut sources, &mut buses, &mut destinations);
    }
    let region = Region::new(GLOBAL);
    let mut present = 0;
    for block in WARM_UP_BLOCKS..WARM_UP_BLOCKS + MEASURED_BLOCKS {
        present = render(block, &mut sources, &mut buses, &mut destinations).strips_present;
    }
    let change = region.change();

    assert_eq!(present, STRIPS);
    assert!(
        buses
            .iter()
            .all(|plane| plane.iter().any(|sample| *sample != 0.0))
    );
    assert!(matrix.bus_meters(MixBus::E)[0].peak > 0.0);
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
