/// Number of independently controllable program recorder slots.
pub const RECORDER_SLOT_COUNT: usize = 2;

/// One independently controllable program recorder, numbered 1..=2.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RecorderSlot(u8);

impl RecorderSlot {
    /// Every recorder slot in ascending slot order.
    pub const ALL: [Self; RECORDER_SLOT_COUNT] = [Self(1), Self(2)];

    /// Creates a recorder slot from its one-based number.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= 1 && value <= 2 {
            Some(Self(value))
        } else {
            None
        }
    }

    /// The one-based slot number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Zero-based array index for this slot.
    pub(crate) const fn index(self) -> usize {
        (self.0 - 1) as usize
    }
}

impl core::fmt::Display for RecorderSlot {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(formatter)
    }
}
