//! Instant replay value types (OR-012).
//!
//! A project holds one [`ReplaySettings`]: which inputs the server records
//! continuously for replay (at most [`MAX_REPLAY_RECORD_SOURCES`]), how many
//! seconds of each it keeps ([`ReplaySettings::retention_seconds`]), and an
//! optional storage root directory for the rolling segment files. The
//! recorded media itself is never part of the project.
//!
//! A Replay input plays a captured event's angle at one of the fixed
//! [`ReplaySpeed`]s.

use std::fmt;

use crate::InputId;

/// Most inputs one project records for replay.
pub const MAX_REPLAY_RECORD_SOURCES: usize = 4;
/// Shortest retention window, in seconds.
pub const MIN_REPLAY_RETENTION_SECONDS: u32 = 10;
/// Longest retention window, in seconds (one hour).
pub const MAX_REPLAY_RETENTION_SECONDS: u32 = 3_600;
/// The retention window of a new project, in seconds (ten minutes).
pub const DEFAULT_REPLAY_RETENTION_SECONDS: u32 = 600;
/// Longest storage root path, in UTF-8 bytes.
pub const MAX_REPLAY_STORAGE_ROOT_BYTES: usize = 1_024;
/// Most Replay inputs one project holds.
pub const MAX_REPLAY_INPUTS: usize = 2;
/// Most replay events kept at once.
pub const MAX_REPLAY_EVENTS: usize = 64;
/// Longest replay event name, in UTF-8 bytes.
pub const MAX_REPLAY_EVENT_NAME_BYTES: usize = 64;
/// The last-N capture lengths an operator can take, in seconds.
pub const REPLAY_LAST_SECONDS: [u8; 3] = [5, 10, 20];
/// Most frames one replay step (jog) moves a player, either way (ten
/// seconds at 60 frames per second).
pub const MAX_REPLAY_STEP_FRAMES: u32 = 600;

/// Whether `frames` is a usable replay step: nonzero and at most
/// [`MAX_REPLAY_STEP_FRAMES`] either way.
#[must_use]
pub const fn replay_step_is_valid(frames: i32) -> bool {
    frames != 0 && frames.unsigned_abs() <= MAX_REPLAY_STEP_FRAMES
}

/// Longest mix back to the live source when a Replay input finishes on
/// Program (ten seconds at 30 frames per second).
pub const MAX_REPLAY_AUTO_RETURN_FRAMES: u32 = 300;

/// What a project records for replay and how long it keeps it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReplaySettings {
    /// The recorded inputs (replay angles), in angle order, distinct. Empty
    /// turns replay recording off.
    pub sources: Vec<InputId>,
    /// Seconds of every source kept on disk behind the live edge, in
    /// [`MIN_REPLAY_RETENTION_SECONDS`]`..=`[`MAX_REPLAY_RETENTION_SECONDS`].
    /// Media a captured event uses is kept until the event is deleted.
    pub retention_seconds: u32,
    /// An absolute directory for the segment files; `None` lets the server
    /// choose (a `replay` directory beside its runtime state).
    pub storage_root: Option<String>,
    /// Auto return: when a Replay input on Program plays its event to the
    /// end, Program goes back to the last non-Replay source it showed, with
    /// a mix of this many frames (0 cuts, at most
    /// [`MAX_REPLAY_AUTO_RETURN_FRAMES`]); `None` leaves the Replay input
    /// on air.
    pub auto_return: Option<u32>,
}

impl Default for ReplaySettings {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            retention_seconds: DEFAULT_REPLAY_RETENTION_SECONDS,
            storage_root: None,
            auto_return: None,
        }
    }
}

/// Why replay settings are refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaySettingsError {
    /// More than [`MAX_REPLAY_RECORD_SOURCES`] sources.
    TooManySources,
    /// A source is listed twice.
    DuplicateSource(InputId),
    /// A source is not an input of the project.
    UnknownSource(InputId),
    /// A source cannot be recorded (a Replay input itself).
    UnrecordableSource(InputId),
    /// The retention window is out of range.
    Retention,
    /// The storage root is empty, too long, relative, or holds control
    /// characters.
    StorageRoot,
    /// The auto return mix is longer than
    /// [`MAX_REPLAY_AUTO_RETURN_FRAMES`].
    AutoReturn,
}

impl fmt::Display for ReplaySettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManySources => write!(
                formatter,
                "replay records at most {MAX_REPLAY_RECORD_SOURCES} inputs"
            ),
            Self::DuplicateSource(input) => {
                write!(formatter, "input {input} is a replay source twice")
            }
            Self::UnknownSource(input) => write!(formatter, "input {input} does not exist"),
            Self::UnrecordableSource(input) => {
                write!(formatter, "input {input} cannot be recorded for replay")
            }
            Self::Retention => write!(
                formatter,
                "replay retention must be {MIN_REPLAY_RETENTION_SECONDS} through \
                 {MAX_REPLAY_RETENTION_SECONDS} seconds"
            ),
            Self::StorageRoot => formatter.write_str(
                "the replay storage root must be an absolute path of at most 1024 bytes",
            ),
            Self::AutoReturn => write!(
                formatter,
                "the replay auto return mixes for at most {MAX_REPLAY_AUTO_RETURN_FRAMES} frames"
            ),
        }
    }
}

impl std::error::Error for ReplaySettingsError {}

/// How an input may take part in replay recording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaySourceCheck {
    /// The input exists and can be recorded.
    Recordable,
    /// The input exists but cannot be recorded.
    Unrecordable,
    /// No such input.
    Unknown,
}

impl ReplaySettings {
    /// Checks the settings against the project's inputs (`check` says how
    /// each source may take part).
    ///
    /// # Errors
    ///
    /// The first [`ReplaySettingsError`] found.
    pub fn check(
        &self,
        mut check: impl FnMut(InputId) -> ReplaySourceCheck,
    ) -> Result<(), ReplaySettingsError> {
        if self.sources.len() > MAX_REPLAY_RECORD_SOURCES {
            return Err(ReplaySettingsError::TooManySources);
        }
        for (index, source) in self.sources.iter().enumerate() {
            if self.sources[..index].contains(source) {
                return Err(ReplaySettingsError::DuplicateSource(*source));
            }
            match check(*source) {
                ReplaySourceCheck::Recordable => {}
                ReplaySourceCheck::Unrecordable => {
                    return Err(ReplaySettingsError::UnrecordableSource(*source));
                }
                ReplaySourceCheck::Unknown => {
                    return Err(ReplaySettingsError::UnknownSource(*source));
                }
            }
        }
        if !(MIN_REPLAY_RETENTION_SECONDS..=MAX_REPLAY_RETENTION_SECONDS)
            .contains(&self.retention_seconds)
        {
            return Err(ReplaySettingsError::Retention);
        }
        if let Some(root) = &self.storage_root
            && !replay_storage_root_is_valid(root)
        {
            return Err(ReplaySettingsError::StorageRoot);
        }
        if self
            .auto_return
            .is_some_and(|frames| frames > MAX_REPLAY_AUTO_RETURN_FRAMES)
        {
            return Err(ReplaySettingsError::AutoReturn);
        }
        Ok(())
    }
}

/// Whether `root` is a usable storage root: non-empty, at most
/// [`MAX_REPLAY_STORAGE_ROOT_BYTES`], absolute (a leading `/`, or a Windows
/// drive or UNC prefix), and free of control characters.
#[must_use]
pub fn replay_storage_root_is_valid(root: &str) -> bool {
    let bytes = root.as_bytes();
    let absolute = root.starts_with('/')
        || root.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'));
    !root.is_empty()
        && root.len() <= MAX_REPLAY_STORAGE_ROOT_BYTES
        && absolute
        && !root.chars().any(char::is_control)
}

/// Whether `name` is a usable replay event name: non-blank, at most
/// [`MAX_REPLAY_EVENT_NAME_BYTES`], and free of control characters.
#[must_use]
pub fn replay_event_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_REPLAY_EVENT_NAME_BYTES
        && !name.chars().any(char::is_control)
}

/// A Replay input's playback speed.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReplaySpeed {
    /// Quarter speed.
    Quarter,
    /// Half speed.
    Half,
    /// Three-quarter speed.
    ThreeQuarters,
    /// Real time.
    #[default]
    Full,
}

impl ReplaySpeed {
    pub const ALL: [Self; 4] = [Self::Full, Self::ThreeQuarters, Self::Half, Self::Quarter];

    /// The speed in percent of real time.
    #[must_use]
    pub const fn percent(self) -> u8 {
        match self {
            Self::Quarter => 25,
            Self::Half => 50,
            Self::ThreeQuarters => 75,
            Self::Full => 100,
        }
    }

    /// Source quarter-frames one output frame advances.
    #[must_use]
    pub const fn quarter_frames(self) -> u64 {
        match self {
            Self::Quarter => 1,
            Self::Half => 2,
            Self::ThreeQuarters => 3,
            Self::Full => 4,
        }
    }

    /// The speed of `percent` (25, 50, 75, or 100).
    #[must_use]
    pub fn from_percent(percent: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|speed| speed.percent() == percent)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use super::*;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    #[test]
    fn settings_are_checked_against_the_inputs() {
        let known = |id: InputId| match id.get().get() {
            1..=5 => ReplaySourceCheck::Recordable,
            9 => ReplaySourceCheck::Unrecordable,
            _ => ReplaySourceCheck::Unknown,
        };
        let mut settings = ReplaySettings {
            sources: vec![input(1), input(2)],
            ..ReplaySettings::default()
        };
        assert_eq!(settings.check(known), Ok(()));
        settings.sources = (1..=5).map(input).collect();
        assert_eq!(
            settings.check(known),
            Err(ReplaySettingsError::TooManySources)
        );
        settings.sources = vec![input(1), input(1)];
        assert_eq!(
            settings.check(known),
            Err(ReplaySettingsError::DuplicateSource(input(1)))
        );
        settings.sources = vec![input(9)];
        assert_eq!(
            settings.check(known),
            Err(ReplaySettingsError::UnrecordableSource(input(9)))
        );
        settings.sources = vec![input(7)];
        assert_eq!(
            settings.check(known),
            Err(ReplaySettingsError::UnknownSource(input(7)))
        );
        settings.sources.clear();
        settings.retention_seconds = MIN_REPLAY_RETENTION_SECONDS - 1;
        assert_eq!(settings.check(known), Err(ReplaySettingsError::Retention));
        settings.retention_seconds = MAX_REPLAY_RETENTION_SECONDS;
        settings.storage_root = Some("relative/dir".to_owned());
        assert_eq!(settings.check(known), Err(ReplaySettingsError::StorageRoot));
        for root in ["/srv/replay", "C:\\Replay", "\\\\nas\\replay"] {
            settings.storage_root = Some(root.to_owned());
            assert_eq!(settings.check(known), Ok(()), "{root}");
        }
    }

    #[test]
    fn speeds_round_trip_their_percent() {
        for speed in ReplaySpeed::ALL {
            assert_eq!(ReplaySpeed::from_percent(speed.percent()), Some(speed));
            assert_eq!(u64::from(speed.percent()), speed.quarter_frames() * 25);
        }
        assert_eq!(ReplaySpeed::from_percent(60), None);
        assert!(replay_event_name_is_valid("Goal 1"));
        assert!(!replay_event_name_is_valid("  "));
        assert!(!replay_event_name_is_valid("a\nb"));
    }
}
