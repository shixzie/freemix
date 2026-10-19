//! The bounded wait a native frame gives a worker-fed source.
//!
//! Before a frame is realized the loop asks each worker-fed source (a media
//! file's projected tick, an animating title's projected pose) whether the
//! frame's input has arrived. A source still pending holds the frame, but
//! only for a bound: [`SourceWait::waited_out`] releases it once the bound
//! has passed after the later of the frame's host deadline and the moment
//! the frame was first found waiting, and counts each frame released that
//! way once.
//!
//! Measuring from the first wait as well as the deadline matters once the
//! loop runs late (an overloaded host, a slow GPU): every deadline is then
//! long past when its frame is projected, so a wait bounded by the deadline
//! alone gives the worker no time at all. Every frame is then realized
//! before its worker delivers: a clip's tick audio arrives after its frame
//! was mixed and is dropped, and an animating title skips its phase poses,
//! for as long as the loop stays behind.

use std::time::{Duration, Instant};

/// How long one engine frame has waited for a worker-fed source, and how
/// many frames were realized without it.
#[derive(Debug)]
pub(crate) struct SourceWait {
    bound: Duration,
    /// The engine frame last found waiting, and when it first was.
    asked: Option<(u64, Instant)>,
    /// The last frame counted in `late_frames`.
    late_frame: Option<u64>,
    late_frames: u64,
}

impl SourceWait {
    /// A wait that holds each frame at most `bound`.
    pub(crate) const fn new(bound: Duration) -> Self {
        Self {
            bound,
            asked: None,
            late_frame: None,
            late_frames: 0,
        }
    }

    /// Whether engine frame `frame`, due at `deadline` and still waiting
    /// for its source at `now`, has waited long enough: until the bound
    /// past the later of its deadline and the first time it was found
    /// waiting. A frame realized without its source is counted once.
    pub(crate) fn waited_out(&mut self, frame: u64, deadline: Instant, now: Instant) -> bool {
        let asked = match self.asked {
            Some((waiting, since)) if waiting == frame => since,
            _ => {
                self.asked = Some((frame, now));
                now
            }
        };
        if now < deadline.max(asked) + self.bound {
            return false;
        }
        if self.late_frame != Some(frame) {
            self.late_frame = Some(frame);
            self.late_frames = self.late_frames.saturating_add(1);
        }
        true
    }

    /// Frames realized before their source arrived.
    pub(crate) const fn late_frames(&self) -> u64 {
        self.late_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUND: Duration = Duration::from_millis(100);

    #[test]
    fn an_on_time_frame_waits_until_the_bound_past_its_deadline() {
        let mut wait = SourceWait::new(BOUND);
        let early = Instant::now();
        let deadline = early + Duration::from_millis(30);
        assert!(!wait.waited_out(9, deadline, early));
        assert!(!wait.waited_out(9, deadline, deadline + Duration::from_millis(99)));
        assert!(wait.waited_out(9, deadline, deadline + BOUND));
        assert_eq!(wait.late_frames(), 1);
    }

    #[test]
    fn a_late_frame_still_waits_the_bound_from_its_first_check() {
        let mut wait = SourceWait::new(BOUND);
        let deadline = Instant::now();
        let now = deadline + Duration::from_secs(8);
        assert!(
            !wait.waited_out(9, deadline, now),
            "waits on its first check"
        );
        assert!(!wait.waited_out(9, deadline, now + Duration::from_millis(50)));
        assert!(wait.waited_out(9, deadline, now + BOUND));
        assert!(wait.waited_out(9, deadline, now + BOUND * 2));
        assert_eq!(wait.late_frames(), 1, "a frame is counted once");
        // The next late frame gets its own wait.
        let later = now + BOUND * 2;
        assert!(!wait.waited_out(10, deadline, later));
        assert!(wait.waited_out(10, deadline, later + BOUND));
        assert_eq!(wait.late_frames(), 2);
    }
}
