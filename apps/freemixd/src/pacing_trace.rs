//! Opt-in per-frame pacing trace of a native daemon.
//!
//! With `FREEMIXD_PACING_TRACE=1` in its environment, a native daemon keeps
//! how late every Program frame it realized started against its pacer
//! deadline, and how long realizing it took, and prints them once at
//! shutdown as one `FREEMIXD_PACING_TRACE` record. The whole-run telemetry
//! keeps only its newest samples; the trace keeps the startup frames too, so
//! a test can tell startup stalls from steady-state ones, and it marks the
//! frames commands were realized on.

use std::time::{Duration, Instant};

/// The environment variable that enables the trace.
pub(crate) const PACING_TRACE_ENV: &str = "FREEMIXD_PACING_TRACE";
/// Frames the trace keeps: an hour at 60 fps. Later frames are counted only.
const MAX_TRACED_FRAMES: usize = 60 * 60 * 60;

/// One realized frame: lateness and realization time, in microseconds, and
/// whether a command was realized on it.
#[derive(Clone, Copy)]
struct FrameSample {
    lateness_us: u32,
    realize_us: u32,
    command: bool,
}

pub(crate) struct PacingTrace {
    samples: Vec<FrameSample>,
    frames: u64,
}

fn micros(duration: Duration) -> u32 {
    u32::try_from(duration.as_micros()).unwrap_or(u32::MAX)
}

impl PacingTrace {
    /// The trace, when the environment asks for one.
    pub(crate) fn from_environment() -> Option<Self> {
        (std::env::var(PACING_TRACE_ENV).as_deref() == Ok("1")).then(|| Self {
            samples: Vec::with_capacity(4_096),
            frames: 0,
        })
    }

    /// Records one frame due at `deadline`, started at `started`, and
    /// realized at `finished`; `command` marks a command's frame.
    pub(crate) fn frame(
        &mut self,
        deadline: Instant,
        started: Instant,
        finished: Instant,
        command: bool,
    ) {
        self.frames = self.frames.saturating_add(1);
        if self.samples.len() < MAX_TRACED_FRAMES {
            self.samples.push(FrameSample {
                lateness_us: micros(started.saturating_duration_since(deadline)),
                realize_us: micros(finished.saturating_duration_since(started)),
                command,
            });
        }
    }

    /// The shutdown record: every traced frame as `lateness:realize`
    /// microseconds, in frame order, with `:c` after a command's frame.
    pub(crate) fn record(&self) -> String {
        let samples = self
            .samples
            .iter()
            .map(|sample| {
                format!(
                    "{}:{}{}",
                    sample.lateness_us,
                    sample.realize_us,
                    if sample.command { ":c" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "FREEMIXD_PACING_TRACE\tv=1\tframes={}\ttraced={}\tsamples={samples}",
            self.frames,
            self.samples.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_lists_every_frame_in_order() {
        let mut trace = PacingTrace {
            samples: Vec::new(),
            frames: 0,
        };
        let deadline = Instant::now();
        trace.frame(
            deadline,
            deadline + Duration::from_micros(250),
            deadline + Duration::from_micros(1_250),
            false,
        );
        trace.frame(deadline, deadline, deadline, true);
        assert_eq!(
            trace.record(),
            "FREEMIXD_PACING_TRACE\tv=1\tframes=2\ttraced=2\tsamples=250:1000,0:0:c"
        );
    }
}
