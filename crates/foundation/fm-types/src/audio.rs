#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SampleFormat {
    I16,
    I24,
    I32,
    F32,
    F64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SampleRate(u32);

impl SampleRate {
    #[must_use]
    pub const fn new(hertz: u32) -> Option<Self> {
        if hertz == 0 { None } else { Some(Self(hertz)) }
    }

    #[must_use]
    pub const fn hertz(self) -> u32 {
        self.0
    }
}

/// One-based discrete (unlabelled) channel number in `1..=16`.
///
/// Discrete channels carry no speaker semantics; they model slots such as SDI
/// embedded audio channels or multitrack interchange tracks.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiscreteChannel(u8);

impl DiscreteChannel {
    /// Largest discrete channel number.
    pub const MAX: u8 = 16;

    /// Returns `None` outside `1..=16`.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= Self::MAX {
            Some(Self(number))
        } else {
            None
        }
    }

    /// The one-based channel number.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

/// Semantic label of one audio channel.
///
/// Surround labels follow ITU-R BS.775 / SMPTE ST 2036-2 naming: in 5.1,
/// `LeftSurround`/`RightSurround` are the surround pair; in 7.1 they are the
/// side surrounds (`Lss`/`Rss`), and `LeftRearSurround`/`RightRearSurround`
/// are the rear (back) surrounds (`Lrs`/`Rrs`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Channel {
    Mono,
    Left,
    Right,
    Center,
    LowFrequency,
    /// Left surround (5.1) or left side surround (7.1).
    LeftSurround,
    /// Right surround (5.1) or right side surround (7.1).
    RightSurround,
    /// Left rear (back) surround, `Lrs`.
    LeftRearSurround,
    /// Right rear (back) surround, `Rrs`.
    RightRearSurround,
    /// Unlabelled discrete channel.
    Discrete(DiscreteChannel),
}

/// An ordered, non-empty list of at most [`ChannelLayout::MAX_CHANNELS`]
/// channel labels.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChannelLayout(Vec<Channel>);

impl ChannelLayout {
    /// Largest channel count a layout may hold.
    pub const MAX_CHANNELS: usize = 16;

    /// Returns `None` for an empty layout or one with more than
    /// [`Self::MAX_CHANNELS`] channels.
    #[must_use]
    pub fn new(channels: Vec<Channel>) -> Option<Self> {
        if channels.is_empty() || channels.len() > Self::MAX_CHANNELS {
            None
        } else {
            Some(Self(channels))
        }
    }

    #[must_use]
    pub fn channels(&self) -> &[Channel] {
        &self.0
    }

    #[must_use]
    pub fn stereo() -> Self {
        Self(vec![Channel::Left, Channel::Right])
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AudioFormat {
    pub sample_rate: SampleRate,
    pub sample_format: SampleFormat,
    pub channels: ChannelLayout,
}

#[cfg(test)]
mod tests {
    use super::{Channel, ChannelLayout, DiscreteChannel};

    #[test]
    fn discrete_channel_numbers_are_one_based_and_bounded() {
        assert_eq!(DiscreteChannel::new(0), None);
        assert_eq!(DiscreteChannel::new(17), None);
        assert_eq!(DiscreteChannel::new(1).unwrap().number(), 1);
        assert_eq!(DiscreteChannel::new(16).unwrap().number(), 16);
    }

    #[test]
    fn layouts_hold_seven_one_and_sixteen_discrete_channels() {
        let seven_one = ChannelLayout::new(vec![
            Channel::Left,
            Channel::Right,
            Channel::Center,
            Channel::LowFrequency,
            Channel::LeftSurround,
            Channel::RightSurround,
            Channel::LeftRearSurround,
            Channel::RightRearSurround,
        ])
        .unwrap();
        assert_eq!(seven_one.channels().len(), 8);

        let discrete: Vec<_> = (1..=16)
            .map(|number| Channel::Discrete(DiscreteChannel::new(number).unwrap()))
            .collect();
        let layout = ChannelLayout::new(discrete.clone()).unwrap();
        assert_eq!(layout.channels(), discrete.as_slice());

        let mut too_many = discrete;
        too_many.push(Channel::Mono);
        assert_eq!(ChannelLayout::new(too_many), None);
        assert_eq!(ChannelLayout::new(Vec::new()), None);
    }
}
