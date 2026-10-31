//! The project's aux buses A–G (AU-002, schema 90).
//!
//! - Every input strip's [`crate::InputAudioStripState::sends`] says whether
//!   it feeds Master and each aux bus, at what level, and whether the send
//!   taps before or after the fader. A strip off Master still meters and
//!   still feeds its aux buses. Bus assignment is a live strip control, like
//!   mute.
//! - A project keeps exactly [`AUX_BUS_COUNT`] aux buses, each with a name
//!   (a project edit) and a live fader and mute.
//! - A project output hears Master unless it names an aux bus.

pub use fm_types::audio_bus::{
    AUX_BUS_COUNT, AudioBusMask, AudioSends, AudioSendsError, AuxBusNumber, BusSendState, SendTap,
};

use crate::InputGainMilliDb;

/// The TCP port aux bus A's OMT audio sender listens on (AU-002); each later
/// bus adds its index, so buses A–G use 9300 through 9306.
pub const AUX_OMT_BASE_PORT: u16 = 9_300;

/// The OMT port aux bus `bus` listens on when it is published.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // an aux bus index is 0 through 6
pub const fn aux_omt_port(bus: AuxBusNumber) -> u16 {
    AUX_OMT_BASE_PORT + bus.index() as u16
}

/// One aux bus's name, fader, mute, DSP chain, and output device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuxBus {
    pub name: String,
    pub gain: InputGainMilliDb,
    pub muted: bool,
    /// The bus's DSP chain (AU-002), after its fader and mute.
    pub dsp: fm_types::strip_dsp::StripDsp,
    /// The local audio output device the bus plays on (AU-002), or `None`
    /// for meter-only.
    pub device: Option<String>,
    /// Whether the bus is published as an OMT audio sender on
    /// [`AUX_OMT_BASE_PORT`](crate::AUX_OMT_BASE_PORT) plus its index
    /// (AU-002, schema 98).
    pub omt: bool,
}

impl AuxBus {
    /// The default aux bus `number`: named "Bus A" through "Bus G", at
    /// unity, unmuted, metered only.
    #[must_use]
    pub fn default_for(number: AuxBusNumber) -> Self {
        Self {
            name: format!("Bus {}", number.letter()),
            gain: InputGainMilliDb::UNITY,
            muted: false,
            dsp: fm_types::strip_dsp::StripDsp::DEFAULT,
            device: None,
            omt: false,
        }
    }
}

/// The project's [`AUX_BUS_COUNT`] aux buses, bus A first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuxBuses([AuxBus; AUX_BUS_COUNT]);

impl Default for AuxBuses {
    fn default() -> Self {
        Self(AuxBusNumber::ALL.map(AuxBus::default_for))
    }
}

impl AuxBuses {
    /// Buses from explicit values, bus A first; validated by
    /// [`crate::Project::validate`].
    #[must_use]
    pub const fn new(buses: [AuxBus; AUX_BUS_COUNT]) -> Self {
        Self(buses)
    }

    #[must_use]
    pub const fn get(&self, bus: AuxBusNumber) -> &AuxBus {
        &self.0[bus.index()]
    }

    pub fn set(&mut self, bus: AuxBusNumber, value: AuxBus) {
        self.0[bus.index()] = value;
    }

    /// Replaces only `bus`'s DSP chain.
    pub fn set_dsp(&mut self, bus: AuxBusNumber, dsp: fm_types::strip_dsp::StripDsp) {
        self.0[bus.index()].dsp = dsp;
    }

    /// Every bus with its number, bus A first.
    pub fn iter(&self) -> impl Iterator<Item = (AuxBusNumber, &AuxBus)> {
        AuxBusNumber::ALL.into_iter().zip(self.0.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_buses_are_lettered() {
        let buses = AuxBuses::default();
        assert_eq!(buses.get(AuxBusNumber::ALL[0]).name, "Bus A");
        assert_eq!(buses.get(AuxBusNumber::ALL[6]).name, "Bus G");
        assert!(buses.iter().all(|(_, bus)| !bus.muted));
    }
}
