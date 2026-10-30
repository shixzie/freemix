//! AU-006: the Master bus plugin stage processes the summed Program bus
//! after the Master DSP chain, before clipping and the Master meter, and a
//! cloned mixer keeps the stage.

use std::fmt;
use std::num::NonZeroU128;

use fm_audio::{AudioBlock, ChannelMapping, InputState, MasterMixer, MasterPluginStage};
use fm_types::{AudioFormat, Channel, ChannelLayout, InputId, SampleFormat, SampleRate};

#[derive(Clone)]
struct GainStage {
    gain: f32,
}

impl fmt::Debug for GainStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GainStage")
            .field("gain", &self.gain)
            .finish()
    }
}

impl MasterPluginStage for GainStage {
    fn process(&mut self, planes: &mut [Vec<f32>], samples: usize) {
        for plane in planes {
            for sample in &mut plane[..samples] {
                *sample *= self.gain;
            }
        }
    }

    fn clone_stage(&self) -> Box<dyn MasterPluginStage> {
        Box::new(self.clone())
    }
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn format() -> AudioFormat {
    AudioFormat {
        sample_rate: SampleRate::new(48_000).unwrap(),
        sample_format: SampleFormat::F32,
        channels: ChannelLayout::new(vec![Channel::Mono]).unwrap(),
    }
}

#[test]
fn the_master_stage_processes_before_the_master_meter_and_survives_clone() {
    let format = format();
    let id = input(1);
    let mut mixer = MasterMixer::new(format.clone()).unwrap();
    mixer
        .add_input(
            id,
            format.clone(),
            ChannelMapping::identity(format.channels.clone()).unwrap(),
            InputState::default(),
        )
        .unwrap();
    let block = AudioBlock::from_planar(format.clone(), vec![vec![0.5_f32; 8]]).unwrap();

    let plain = mixer.mix(8, &[(id, &block)], None).unwrap();
    assert!((plain.block.planes()[0][0] - 0.5).abs() < 1e-6);
    assert!((plain.meters.channels()[0].peak - 0.5).abs() < 1e-6);

    // -12 dB is about 0.251; both Program and its meter carry the staged
    // signal.
    mixer.set_master_plugins(Some(Box::new(GainStage { gain: 0.25 })));
    assert!(mixer.has_master_plugins());
    let staged = mixer.mix(8, &[(id, &block)], None).unwrap();
    assert!((staged.block.planes()[0][0] - 0.125).abs() < 1e-6);
    assert!((staged.meters.channels()[0].peak - 0.125).abs() < 1e-6);

    // A clone (the daemon's pending mixer) keeps the stage and its state.
    let mut clone = mixer.clone();
    assert!(clone.has_master_plugins());
    let cloned = clone.mix(8, &[(id, &block)], None).unwrap();
    assert!((cloned.block.planes()[0][0] - 0.125).abs() < 1e-6);

    mixer.set_master_plugins(None);
    let cleared = mixer.mix(8, &[(id, &block)], None).unwrap();
    assert!((cleared.block.planes()[0][0] - 0.5).abs() < 1e-6);
}
