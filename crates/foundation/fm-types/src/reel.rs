//! Highlight reel value types (OR-013).
//!
//! A project holds at most [`MAX_REELS`] [`ReelSpec`]s. A reel is an ordered
//! list of at most [`MAX_REEL_CLIPS`] clips cut from recorded replay events:
//! each clip names the event, the recorded angle it plays, trims before and
//! after the event marks, a playback speed, and how many frames it dissolves
//! in from the clip before (zero for a cut, always on the first clip). The
//! recorded media itself is never part of the project.

use std::fmt;

use crate::{InputId, replay::ReplaySpeed};

/// Most reels one project holds.
pub const MAX_REELS: usize = 8;
/// Most clips one reel holds.
pub const MAX_REEL_CLIPS: usize = 64;
/// Longest reel name, in UTF-8 bytes.
pub const MAX_REEL_NAME_BYTES: usize = 64;
/// Longest reel export path, in UTF-8 bytes.
pub const MAX_REEL_PATH_BYTES: usize = 1_024;
/// Largest trim before or after an event mark, in frames (one hour at 60
/// frames per second).
pub const MAX_REEL_TRIM_FRAMES: u32 = 3_600 * 60;
/// Longest dissolve between two clips, in frames (ten seconds at 60 frames
/// per second).
pub const MAX_REEL_DISSOLVE_FRAMES: u32 = 600;

/// One highlight reel: an ordered selection of replay-event clips.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReelSpec {
    /// The reel's id, distinct within the project.
    pub id: u64,
    /// The operator label.
    pub name: String,
    /// The clips, in play order.
    pub clips: Vec<ReelClip>,
}

/// One clip of a highlight reel.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReelClip {
    /// The replay event the clip cuts from.
    pub event: u32,
    /// The recorded input (angle) the clip plays.
    pub angle: InputId,
    /// Frames kept before the event's in mark.
    pub trim_in_frames: u32,
    /// Frames kept after the event's out mark.
    pub trim_out_frames: u32,
    /// Playback speed in percent of real time: 25, 50, 75, or 100.
    pub speed_percent: u8,
    /// Frames this clip dissolves in over the end of the clip before; zero
    /// is a cut, and the first clip always cuts.
    pub dissolve_frames: u32,
}

/// Why a reel is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReelSpecError {
    /// The project already holds [`MAX_REELS`] reels.
    TooManyReels,
    /// The reel holds more than [`MAX_REEL_CLIPS`] clips.
    TooManyClips,
    /// The reel holds no clips.
    EmptyReel,
    /// Another reel already uses this id.
    DuplicateReel(u64),
    /// The name is blank, too long, or holds control characters.
    Name,
    /// The angle is not an input of the project or cannot be recorded (a
    /// Replay input itself).
    UnrecordedAngle(InputId),
    /// The speed is not 25, 50, 75, or 100 percent.
    Speed(u8),
    /// A trim is larger than [`MAX_REEL_TRIM_FRAMES`].
    Trim,
    /// The first clip dissolves, or a dissolve is longer than
    /// [`MAX_REEL_DISSOLVE_FRAMES`].
    Dissolve,
}

impl fmt::Display for ReelSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyReels => {
                write!(formatter, "a project holds at most {MAX_REELS} reels")
            }
            Self::TooManyClips => {
                write!(formatter, "a reel holds at most {MAX_REEL_CLIPS} clips")
            }
            Self::EmptyReel => formatter.write_str("a reel must hold at least one clip"),
            Self::DuplicateReel(id) => write!(formatter, "reel {id} occurs more than once"),
            Self::Name => write!(
                formatter,
                "a reel name must be non-blank, at most {MAX_REEL_NAME_BYTES} bytes, and free of \
                 control characters"
            ),
            Self::UnrecordedAngle(input) => {
                write!(formatter, "input {input} cannot be used as a reel angle")
            }
            Self::Speed(percent) => write!(
                formatter,
                "reel clip speed must be 25, 50, 75, or 100 percent, got {percent}"
            ),
            Self::Trim => write!(
                formatter,
                "reel clip trims must be at most {MAX_REEL_TRIM_FRAMES} frames"
            ),
            Self::Dissolve => write!(
                formatter,
                "the first reel clip must cut, and a dissolve must be at most \
                 {MAX_REEL_DISSOLVE_FRAMES} frames"
            ),
        }
    }
}

impl std::error::Error for ReelSpecError {}

impl ReelSpec {
    /// Checks the reel against the project's inputs (`is_recorded` says
    /// whether an angle is an input that can be recorded).
    ///
    /// # Errors
    ///
    /// The first [`ReelSpecError`] found. The project-wide reel count and
    /// duplicate ids are checked by `Project::set_reel` and project
    /// validation.
    pub fn check(&self, is_recorded: impl Fn(InputId) -> bool) -> Result<(), ReelSpecError> {
        if self.clips.len() > MAX_REEL_CLIPS {
            return Err(ReelSpecError::TooManyClips);
        }
        if self.clips.is_empty() {
            return Err(ReelSpecError::EmptyReel);
        }
        if !reel_name_is_valid(&self.name) {
            return Err(ReelSpecError::Name);
        }
        for (index, clip) in self.clips.iter().enumerate() {
            if !clip_dissolve_is_valid(index, clip.dissolve_frames) {
                return Err(ReelSpecError::Dissolve);
            }
            if !is_recorded(clip.angle) {
                return Err(ReelSpecError::UnrecordedAngle(clip.angle));
            }
            if ReplaySpeed::from_percent(clip.speed_percent).is_none() {
                return Err(ReelSpecError::Speed(clip.speed_percent));
            }
            if clip.trim_in_frames > MAX_REEL_TRIM_FRAMES
                || clip.trim_out_frames > MAX_REEL_TRIM_FRAMES
            {
                return Err(ReelSpecError::Trim);
            }
        }
        Ok(())
    }
}

/// Whether clip `index` of a reel may dissolve in over `frames`: the first
/// clip cuts, the others dissolve for at most [`MAX_REEL_DISSOLVE_FRAMES`].
#[must_use]
pub const fn clip_dissolve_is_valid(index: usize, frames: u32) -> bool {
    if index == 0 {
        frames == 0
    } else {
        frames <= MAX_REEL_DISSOLVE_FRAMES
    }
}

/// Whether `name` is a usable reel name: non-blank, at most
/// [`MAX_REEL_NAME_BYTES`], and free of control characters.
#[must_use]
pub fn reel_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_REEL_NAME_BYTES
        && !name.chars().any(char::is_control)
}

/// Whether `path` is a usable reel export path: non-empty, at most
/// [`MAX_REEL_PATH_BYTES`], absolute (a leading `/`, or a Windows drive or
/// UNC prefix), and free of control characters.
#[must_use]
pub fn reel_export_path_is_valid(path: &str) -> bool {
    let bytes = path.as_bytes();
    let absolute = path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'));
    !path.is_empty()
        && path.len() <= MAX_REEL_PATH_BYTES
        && absolute
        && !path.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use super::*;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn clip(event: u32, angle: InputId) -> ReelClip {
        ReelClip {
            event,
            angle,
            trim_in_frames: 0,
            trim_out_frames: 0,
            speed_percent: 100,
            dissolve_frames: 0,
        }
    }

    #[test]
    fn reels_are_checked_against_recorded_inputs() {
        let recorded = |id: InputId| matches!(id.get().get(), 1..=5);
        let mut reel = ReelSpec {
            id: 1,
            name: "Highlights".to_owned(),
            clips: vec![clip(7, input(1)), clip(8, input(2))],
        };
        assert_eq!(reel.check(recorded), Ok(()));
        reel.clips.clear();
        assert_eq!(reel.check(recorded), Err(ReelSpecError::EmptyReel));
        reel.clips = (0..=MAX_REEL_CLIPS)
            .map(|event| clip(u32::try_from(event).unwrap(), input(1)))
            .collect();
        assert_eq!(reel.check(recorded), Err(ReelSpecError::TooManyClips));
        reel.clips = vec![clip(7, input(9))];
        assert_eq!(
            reel.check(recorded),
            Err(ReelSpecError::UnrecordedAngle(input(9)))
        );
        reel.clips = vec![ReelClip {
            speed_percent: 60,
            ..clip(7, input(1))
        }];
        assert_eq!(reel.check(recorded), Err(ReelSpecError::Speed(60)));
        reel.clips = vec![ReelClip {
            trim_in_frames: MAX_REEL_TRIM_FRAMES + 1,
            ..clip(7, input(1))
        }];
        assert_eq!(reel.check(recorded), Err(ReelSpecError::Trim));
        reel.clips = vec![ReelClip {
            trim_out_frames: MAX_REEL_TRIM_FRAMES,
            ..clip(7, input(1))
        }];
        assert_eq!(reel.check(recorded), Ok(())); // The first clip cuts; a later one dissolves for at most
        // MAX_REEL_DISSOLVE_FRAMES.
        reel.clips = vec![ReelClip {
            dissolve_frames: 1,
            ..clip(7, input(1))
        }];
        assert_eq!(reel.check(recorded), Err(ReelSpecError::Dissolve));
        reel.clips = vec![
            clip(7, input(1)),
            ReelClip {
                dissolve_frames: MAX_REEL_DISSOLVE_FRAMES,
                ..clip(8, input(2))
            },
        ];
        assert_eq!(reel.check(recorded), Ok(()));
        reel.clips[1].dissolve_frames = MAX_REEL_DISSOLVE_FRAMES + 1;
        assert_eq!(reel.check(recorded), Err(ReelSpecError::Dissolve));
    }

    #[test]
    fn reel_names_and_export_paths_are_bounded() {
        assert!(reel_name_is_valid("Goal 1"));
        assert!(!reel_name_is_valid("  "));
        assert!(!reel_name_is_valid("a\nb"));
        assert!(!reel_name_is_valid(&"a".repeat(MAX_REEL_NAME_BYTES + 1)));
        for path in [
            "/srv/reels/goals.mp4",
            "C:\\Reels\\goals.mp4",
            "\\\\nas\\reels",
        ] {
            assert!(reel_export_path_is_valid(path), "{path}");
        }
        for path in ["", "relative/goals.mp4", "srv/goals.mp4", "/srv/a\nb"] {
            assert!(!reel_export_path_is_valid(path), "{path}");
        }
        assert!(!reel_export_path_is_valid(&format!(
            "/{}",
            "a".repeat(MAX_REEL_PATH_BYTES)
        )));
    }
}
